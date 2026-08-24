use std::{
    fs::{self, File},
    io::{BufRead, BufReader},
    path::Path,
};

use anyhow::{anyhow, bail, Context, Result};
use ctx_history_capture::{
    ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    ProviderSourceStatus,
};
use ctx_history_core::{CaptureProvider, CtxHistoryJsonlRecord, CTX_HISTORY_JSONL_SCHEMA_VERSION};

use super::HistorySourcePluginSource;

const ROUTE_SOURCE_FORMAT: &str = "ctx_history_jsonl_v2";
const PUBLIC_HISTORY_SCHEMA_VERSION: &str = CTX_HISTORY_JSONL_SCHEMA_VERSION;
const MAX_HEADER_BYTES: usize = 1024 * 1024;
const MAX_HEADER_RECORDS: usize = 64;
const MAX_HEADER_LINE_BYTES: usize = 256 * 1024;

pub const COMMAND_ONLY_UNSUPPORTED_REASON: &str =
    "command-only history source plugins are unsupported in 1.0 because command stdout is not a provider-owned durable source; declare a durable path instead";

#[derive(Debug)]
pub struct PreparedHistorySourcePluginRefresh {
    provider_source: ProviderSource,
}

impl PreparedHistorySourcePluginRefresh {
    pub fn provider_source(&self) -> &ProviderSource {
        &self.provider_source
    }

    #[cfg(test)]
    pub fn source_path(&self) -> &Path {
        &self.provider_source.path
    }
}

pub fn prepare_source_backed_history_source(
    source: HistorySourcePluginSource,
    reset_cursor: bool,
) -> Result<PreparedHistorySourcePluginRefresh> {
    if reset_cursor {
        bail!(
            "history source plugin {} uses a durable provider-owned source and has no ctx cursor to reset",
            source.label()
        );
    }
    let source_path = source.source_path.clone().ok_or_else(|| {
        anyhow!(
            "history source plugin {} is unsupported: {COMMAND_ONLY_UNSUPPORTED_REASON}",
            source.label()
        )
    })?;
    validate_provider_owned_source(&source, &source_path)?;
    Ok(PreparedHistorySourcePluginRefresh {
        provider_source: ProviderSource {
            provider: CaptureProvider::Custom,
            path: source_path,
            exists: true,
            source_format: ROUTE_SOURCE_FORMAT,
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Explicit,
            catalog_support: ProviderCatalogSupport::None,
            status: ProviderSourceStatus::Available,
            unsupported_reason: None,
        },
    })
}

fn validate_provider_owned_source(
    source: &HistorySourcePluginSource,
    source_path: &Path,
) -> Result<()> {
    let metadata = fs::symlink_metadata(source_path).with_context(|| {
        format!(
            "inspect durable history source plugin path {}",
            source_path.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        bail!(
            "history source plugin {} durable path {} must be a regular non-symlink file",
            source.label(),
            source_path.display()
        );
    }

    let file = File::open(source_path).with_context(|| {
        format!(
            "open durable history source plugin path {}",
            source_path.display()
        )
    })?;
    let mut reader = BufReader::new(file);
    let mut total_bytes = 0_usize;
    let mut manifest_seen = false;
    let mut source_seen = false;
    let mut line = Vec::new();
    for line_number in 1..=MAX_HEADER_RECORDS {
        line.clear();
        let bytes = reader
            .read_until(b'\n', &mut line)
            .with_context(|| format!("read {}", source_path.display()))?;
        if bytes == 0 {
            break;
        }
        total_bytes = total_bytes.saturating_add(bytes);
        if total_bytes > MAX_HEADER_BYTES || line.len() > MAX_HEADER_LINE_BYTES {
            bail!(
                "history source plugin {} durable source header exceeds the bounded validation window",
                source.label()
            );
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let record: CtxHistoryJsonlRecord = serde_json::from_slice(&line).with_context(|| {
            format!(
                "history source plugin {} durable source has invalid ctx-history-jsonl-v2 at line {line_number}",
                source.label()
            )
        })?;
        match record {
            CtxHistoryJsonlRecord::Manifest(record) => {
                if record.schema_version != PUBLIC_HISTORY_SCHEMA_VERSION {
                    bail!(
                        "history source plugin {} durable source has unsupported schema_version `{}`",
                        source.label(),
                        record.schema_version
                    );
                }
                if record.lineage_contract != source.lineage_contract {
                    bail!(
                        "history source plugin {} lineage_contract does not match its durable source",
                        source.label()
                    );
                }
                manifest_seen = true;
            }
            CtxHistoryJsonlRecord::Source(record)
                if record.provider_key == source.provider_key
                    && record.source_id == source.source_id
                    && record.source_format == source.source_format =>
            {
                source_seen = true;
            }
            _ => {}
        }
        if manifest_seen && source_seen {
            return Ok(());
        }
    }
    if !manifest_seen {
        bail!(
            "history source plugin {} durable source must declare its manifest within the first {MAX_HEADER_RECORDS} records and {MAX_HEADER_BYTES} bytes",
            source.label()
        );
    }
    bail!(
        "history source plugin {} durable source must declare selected identity {}/{}/{} within the first {MAX_HEADER_RECORDS} records and {MAX_HEADER_BYTES} bytes",
        source.label(),
        source.provider_key,
        source.source_id,
        source.source_format
    )
}

#[cfg(test)]
mod tests {
    use std::{io::Write, path::PathBuf};

    use serde_json::json;

    use super::*;

    fn source(path: PathBuf) -> HistorySourcePluginSource {
        HistorySourcePluginSource {
            plugin_name: "example".to_owned(),
            plugin_display_name: None,
            plugin_version: None,
            manifest_path: path.with_extension("manifest.json"),
            id: "default".to_owned(),
            display_name: None,
            provider_key: "example".to_owned(),
            source_id: "default".to_owned(),
            source_format: "example-v1".to_owned(),
            source_path: Some(path),
            lineage_contract: None,
            enabled: true,
            refresh: super::super::HistorySourcePluginRefresh::Manual,
        }
    }

    #[test]
    fn durable_source_identity_is_validated_without_copying_content() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history.jsonl");
        let mut file = File::create(&path).unwrap();
        writeln!(
            file,
            "{}",
            json!({"record_type":"manifest","schema_version":PUBLIC_HISTORY_SCHEMA_VERSION})
        )
        .unwrap();
        writeln!(file, "{}", json!({"record_type":"source","provider_key":"example","source_id":"default","source_format":"example-v1"})).unwrap();

        let prepared = prepare_source_backed_history_source(source(path.clone()), false).unwrap();
        assert_eq!(prepared.source_path(), path);
        assert_eq!(
            prepared.provider_source().source_format,
            ROUTE_SOURCE_FORMAT
        );
        assert!(!temp.path().join("history-source-plugin-sources").exists());
    }

    #[test]
    fn durable_multi_route_source_finds_selected_identity_after_other_routes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history.jsonl");
        let mut file = File::create(&path).unwrap();
        writeln!(
            file,
            "{}",
            json!({"record_type":"manifest","schema_version":PUBLIC_HISTORY_SCHEMA_VERSION})
        )
        .unwrap();
        writeln!(file, "{}", json!({"record_type":"source","provider_key":"other","source_id":"archive","source_format":"other-v1"})).unwrap();
        writeln!(file, "{}", json!({"record_type":"source","provider_key":"example","source_id":"default","source_format":"example-v1"})).unwrap();

        let prepared = prepare_source_backed_history_source(source(path.clone()), false).unwrap();
        assert_eq!(prepared.source_path(), path);
    }

    #[test]
    fn command_only_source_is_typed_unsupported() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history.jsonl");
        let mut source = source(path);
        source.source_path = None;
        let error = prepare_source_backed_history_source(source, false).unwrap_err();
        assert!(error.to_string().contains(COMMAND_ONLY_UNSUPPORTED_REASON));
        assert!(!temp.path().join("history-source-plugin-sources").exists());
    }

    #[test]
    fn unsupported_v1_schema_is_rejected_without_fallback() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history.jsonl");
        let mut file = File::create(&path).unwrap();
        writeln!(
            file,
            "{}",
            json!({"record_type":"manifest","schema_version":"ctx-history-jsonl-v1"})
        )
        .unwrap();
        writeln!(file, "{}", json!({"record_type":"source","provider_key":"example","source_id":"default","source_format":"example-v1"})).unwrap();

        let error = prepare_source_backed_history_source(source(path), false).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unsupported schema_version `ctx-history-jsonl-v1`"),
            "{error:#}"
        );
    }

    #[test]
    fn durable_source_lineage_contract_must_match_manifest() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history.jsonl");
        let mut file = File::create(&path).unwrap();
        writeln!(file, "{}", json!({"record_type":"manifest","schema_version":PUBLIC_HISTORY_SCHEMA_VERSION,"lineage_contract":"provider_native_v1"})).unwrap();
        writeln!(file, "{}", json!({"record_type":"source","provider_key":"example","source_id":"default","source_format":"example-v1"})).unwrap();

        let error = prepare_source_backed_history_source(source(path.clone()), false).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("lineage_contract does not match"),
            "{error:#}"
        );

        let mut matched = source(path);
        matched.lineage_contract =
            Some(ctx_history_core::CtxHistoryJsonlLineageContract::ProviderNativeV1);
        assert!(prepare_source_backed_history_source(matched, false).is_ok());
    }
}
