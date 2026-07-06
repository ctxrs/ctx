use std::{fs::File, io::BufReader, path::Path};

use ctx_history_core::CaptureProvider;

use crate::common::io::{ensure_regular_provider_transcript_file, read_provider_jsonl_line};
use crate::provider::adapter::ProviderFixtureJsonlAdapter;
use crate::provider::importer::fixture_line_to_capture;
use crate::{
    ProviderAdapterContext, ProviderCaptureAdapter, ProviderFixtureLine, ProviderImportFailure,
    ProviderNormalizationResult, Result,
};

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
