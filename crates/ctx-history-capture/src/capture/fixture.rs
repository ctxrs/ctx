#[allow(unused_imports)]
use super::*;

#[derive(Debug, Clone)]
pub struct FixtureOptions {
    pub title: String,
    pub body: String,
    pub tags: Vec<String>,
    pub dedupe_key: Option<String>,
    pub machine_id: Option<String>,
    pub cwd: Option<PathBuf>,
    pub occurred_at: DateTime<Utc>,
}

impl Default for FixtureOptions {
    fn default() -> Self {
        Self {
            title: "Fixture capture".to_owned(),
            body: "fixture body".to_owned(),
            tags: vec!["fixture".to_owned()],
            dedupe_key: None,
            machine_id: None,
            cwd: None,
            occurred_at: utc_now(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProviderFixtureImportOptions {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub history_record_id: Option<Uuid>,
    pub expected_provider: Option<CaptureProvider>,
    pub allow_partial_failures: bool,
    pub source_format: String,
    pub fidelity: Fidelity,
}

impl Default for ProviderFixtureImportOptions {
    fn default() -> Self {
        Self {
            machine_id: default_machine_id(),
            source_path: None,
            imported_at: utc_now(),
            history_record_id: None,
            expected_provider: None,
            allow_partial_failures: false,
            source_format: "normalized_provider_fixture_jsonl".to_owned(),
            fidelity: Fidelity::Imported,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderFixtureLine {
    pub provider: CaptureProvider,
    pub session: ProviderSessionDto,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<ProviderEventDto>,
}

#[derive(Debug, Clone)]
pub struct ProviderFixtureJsonlAdapter {
    pub expected_provider: Option<CaptureProvider>,
    pub source_format: String,
    pub fidelity: Fidelity,
}

impl ProviderCaptureAdapter for ProviderFixtureJsonlAdapter {
    fn provider(&self) -> CaptureProvider {
        self.expected_provider.unwrap_or(CaptureProvider::Unknown)
    }

    fn source_format(&self) -> &str {
        &self.source_format
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        ensure_regular_provider_transcript_file(path)?;
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        let mut result = ProviderNormalizationResult::default();
        let mut line = Vec::new();
        let mut line_number = 0usize;

        while read_provider_jsonl_line(&mut reader, &mut line)? {
            line_number += 1;
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }

            let fixture: ProviderFixtureLine = match serde_json::from_slice(&line) {
                Ok(fixture) => fixture,
                Err(err) => {
                    result.summary.failed += 1;
                    result.summary.failures.push(ProviderImportFailure {
                        line: line_number,
                        error: err.to_string(),
                    });
                    continue;
                }
            };
            if let Some(expected_provider) = self.expected_provider {
                if fixture.provider != expected_provider {
                    result.summary.failed += 1;
                    result.summary.failures.push(ProviderImportFailure {
                        line: line_number,
                        error: format!(
                            "provider fixture line {line_number} has provider `{}` but expected `{}`",
                            fixture.provider.as_str(),
                            expected_provider.as_str()
                        ),
                    });
                    continue;
                }
            }

            result.captures.push((
                line_number,
                fixture_line_to_capture(&fixture, context, &self.source_format, self.fidelity),
            ));
        }

        Ok(result)
    }
}

pub fn write_fixture(inbox: impl AsRef<Path>, options: FixtureOptions) -> Result<PathBuf> {
    let envelope = fixture_envelope(options)?;
    let mut writer = SpoolWriter::create(inbox, &envelope.source.machine_id)?;
    writer.write_envelope(&envelope)?;
    writer.finish()
}

pub fn fixture_envelope(options: FixtureOptions) -> Result<CaptureEnvelope> {
    let machine_id = options.machine_id.unwrap_or_else(default_machine_id);
    let cwd_path = match options.cwd {
        Some(path) => path,
        None => env::current_dir()?,
    };
    let cwd = cwd_path.display().to_string();
    let dedupe_key = options
        .dedupe_key
        .unwrap_or_else(|| format!("fixture:{}", new_id()));
    let tags = if options.tags.is_empty() {
        vec!["fixture".to_owned()]
    } else {
        options.tags
    };
    let payload = json!({
        "kind": "history_record",
        "title": options.title,
        "body": options.body,
        "tags": tags,
        "record_kind": "capture-fixture",
        "workspace": cwd,
    });
    let payload_hash = Some(compute_payload_hash(&payload)?);

    Ok(CaptureEnvelope {
        schema_version: CAPTURE_SCHEMA_VERSION,
        capture_event_id: new_id(),
        dedupe_key,
        source: CaptureSourceDescriptor {
            kind: CaptureSourceKind::DirectCli,
            provider: CaptureProvider::Unknown,
            machine_id,
            process_id: Some(std::process::id()),
            cwd: Some(cwd.clone()),
            raw_source_path: None,
            external_session_id: None,
        },
        occurred_at: options.occurred_at,
        cwd: Some(cwd),
        env_session_hints: json!({}),
        payload,
        payload_hash,
        fidelity: Fidelity::Imported,
    })
}

pub fn import_provider_fixture_jsonl(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: ProviderFixtureImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = ProviderFixtureJsonlAdapter {
        expected_provider: options.expected_provider,
        source_format: options.source_format.clone(),
        fidelity: options.fidelity,
    }
    .normalize_path(
        path,
        &ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: Some(source_path),
            imported_at: options.imported_at,
            tool_output_mode: CodexToolOutputMode::Full,
            event_mode: CodexEventImportMode::Rich,
            include_notices: true,
        },
    )?;

    import_normalized_provider_captures(
        store,
        normalization,
        NormalizedProviderImportOptions {
            history_record_id: options.history_record_id,
            allow_partial_failures: options.allow_partial_failures,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: true,
        },
    )
}
