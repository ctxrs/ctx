use std::io::{Read, Seek, SeekFrom};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::captured_batch::jsonl::VerifiedJsonlAppend;
use crate::common::io::{read_provider_jsonl_line_or_skip_oversized, ProviderJsonlLineRead};
use crate::provider::importer::CapturedBatchCursorMode;
use crate::{CaptureError, ProviderImportSummary, Result, MAX_PROVIDER_JSONL_LINE_BYTES};

use super::header::{bounded_codex_header, codex_session_header, CodexSessionHeader};
use super::CODEX_HEADER_ANCHOR_DOMAIN;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CodexHeaderAnchor {
    #[serde(rename = "s", alias = "start_offset")]
    pub(super) start_offset: u64,
    #[serde(rename = "e", alias = "end_offset")]
    pub(super) end_offset: u64,
    #[serde(rename = "b", alias = "payload_bytes")]
    pub(super) payload_bytes: u64,
    #[serde(rename = "h", alias = "sha256")]
    pub(super) sha256: [u8; 32],
}

pub(super) fn codex_header_anchor(
    start_offset: u64,
    end_offset: u64,
    payload: &[u8],
) -> Result<CodexHeaderAnchor> {
    if start_offset > end_offset {
        return Err(CaptureError::SystemInvariant(
            "Codex session_meta anchor range is invalid",
        ));
    }
    let payload_bytes = u64::try_from(payload.len()).map_err(|_| {
        CaptureError::SystemInvariant("Codex session_meta payload length exceeds u64")
    })?;
    let mut hasher = Sha256::new();
    hasher.update(CODEX_HEADER_ANCHOR_DOMAIN);
    hasher.update(payload_bytes.to_be_bytes());
    hasher.update(payload);
    Ok(CodexHeaderAnchor {
        start_offset,
        end_offset,
        payload_bytes,
        sha256: hasher.finalize().into(),
    })
}

pub(super) fn codex_legacy_cursor_next_ordinal(cursor: &str) -> Result<u64> {
    let line_number = cursor
        .strip_prefix("line:")
        .and_then(|line| line.parse::<u64>().ok())
        .filter(|line| *line != 0)
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Codex tail import cannot resume an unknown legacy cursor".to_owned(),
            )
        })?;
    Ok(line_number)
}

pub(super) fn validate_codex_tail_start_boundary(
    reader: &mut (impl Read + Seek),
    offset: u64,
) -> Result<()> {
    if offset == 0 {
        return Ok(());
    }
    reader.seek(SeekFrom::Start(offset - 1))?;
    let mut previous = [0_u8; 1];
    reader.read_exact(&mut previous)?;
    if previous[0] != b'\n' {
        return Err(CaptureError::InvalidPayload(
            "Codex tail offset is not a complete JSONL record boundary".to_owned(),
        ));
    }
    Ok(())
}

pub(super) enum CodexTailHeaderBootstrap {
    Ready {
        header: Box<CodexSessionHeader>,
        header_end: u64,
        header_anchor: CodexHeaderAnchor,
    },
    Skipped(ProviderImportSummary),
}

pub(super) fn read_codex_tail_header(
    reader: &mut (impl std::io::BufRead + Seek),
) -> Result<CodexTailHeaderBootstrap> {
    reader.seek(SeekFrom::Start(0))?;
    let mut line = Vec::new();
    match read_provider_jsonl_line_or_skip_oversized(reader, &mut line)? {
        ProviderJsonlLineRead::Eof => Err(CaptureError::InvalidPayload(
            "Codex tail source is missing session_meta".to_owned(),
        )),
        ProviderJsonlLineRead::Oversized { .. } => {
            Ok(CodexTailHeaderBootstrap::Skipped(ProviderImportSummary {
                skipped: 1,
                skipped_sessions: 1,
                ..ProviderImportSummary::default()
            }))
        }
        ProviderJsonlLineRead::Line { bytes } => {
            let header = codex_session_header(serde_json::from_slice(&line)?)?;
            let header = bounded_codex_header(header).map_err(|_| {
                CaptureError::InvalidPayload(
                    "Codex session metadata exceeds the bounded parser state limit".to_owned(),
                )
            })?;
            let payload = line.strip_suffix(b"\n").unwrap_or(&line);
            let payload = payload.strip_suffix(b"\r").unwrap_or(payload);
            let header_end = u64::try_from(bytes).map_err(|_| {
                CaptureError::SystemInvariant("Codex session_meta length exceeds u64")
            })?;
            let header_anchor = codex_header_anchor(0, header_end, payload)?;
            Ok(CodexTailHeaderBootstrap::Ready {
                header: Box::new(header),
                header_end,
                header_anchor,
            })
        }
    }
}

pub(super) fn read_codex_anchored_header(
    reader: &mut (impl Read + Seek),
    expected: &CodexHeaderAnchor,
) -> Result<Option<CodexSessionHeader>> {
    let record_bytes = expected
        .end_offset
        .checked_sub(expected.start_offset)
        .ok_or_else(|| {
            CaptureError::InvalidPayload("Codex session_meta anchor range is invalid".to_owned())
        })?;
    let maximum_record_bytes = u64::try_from(MAX_PROVIDER_JSONL_LINE_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(2);
    if record_bytes > maximum_record_bytes {
        return Err(CaptureError::InvalidPayload(
            "Codex session_meta anchor exceeds the bounded record limit".to_owned(),
        ));
    }
    let record_bytes = usize::try_from(record_bytes).map_err(|_| {
        CaptureError::InvalidPayload("Codex session_meta anchor exceeds platform limits".to_owned())
    })?;
    reader.seek(SeekFrom::Start(expected.start_offset))?;
    let mut record = vec![0_u8; record_bytes];
    reader.read_exact(&mut record)?;
    let payload = record.strip_suffix(b"\n").unwrap_or(&record);
    let payload = payload.strip_suffix(b"\r").unwrap_or(payload);
    let observed = codex_header_anchor(expected.start_offset, expected.end_offset, payload)?;
    if &observed != expected {
        return Ok(None);
    }
    let header = codex_session_header(serde_json::from_slice(payload)?)?;
    bounded_codex_header(header).map(Some).map_err(|_| {
        CaptureError::InvalidPayload(
            "Codex session metadata exceeds the bounded parser state limit".to_owned(),
        )
    })
}

pub(super) fn codex_verified_append_cursor_mode(
    verified_append: VerifiedJsonlAppend,
) -> CapturedBatchCursorMode {
    CapturedBatchCursorMode::ResumeAppend(verified_append)
}
