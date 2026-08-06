use std::{
    fs::{File, Metadata},
    io::{Read, Seek, SeekFrom},
    time::UNIX_EPOCH,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::*;

#[derive(Debug, Clone)]
pub(super) struct CodexPromptHistoryFrozenSnapshotV0 {
    pub(super) metadata: Metadata,
    pub(super) ordinary_file_token: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct CheckpointV0 {
    pub(super) version: u32,
    pub(super) certified_prefix_bytes: u64,
    pub(super) complete_records: u64,
    pub(super) terminal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ObservationWireV0 {
    length: u64,
    modified_after_epoch: bool,
    modified_seconds: u64,
    modified_nanos: u32,
    readonly: bool,
    ordinary_file_token: [u8; 32],
    whole_source_digest: [u8; 32],
}

pub(super) fn opened_file_from_start(
    source: &OpenedProviderSourceFile,
) -> CodexPromptHistorySourceBackedResultV0<File> {
    let mut file = source.file().try_clone()?;
    file.seek(SeekFrom::Start(0))?;
    Ok(file)
}

fn hash_opened_prefix(
    source: &OpenedProviderSourceFile,
    target: u64,
) -> CodexPromptHistorySourceBackedResultV0<Option<[u8; 32]>> {
    for _ in 0..2 {
        source.revalidate_same_object()?;
        let before = source.current_ordinary_file_token()?;
        let observed_len = source.file().metadata()?.len();
        let before_hash = source.current_ordinary_file_token()?;
        if before != before_hash {
            continue;
        }

        let digest = read_opened_prefix(source, target, observed_len)?;
        let confirmation = read_opened_prefix(source, target, observed_len)?;
        if digest != confirmation {
            return Err(CodexPromptHistorySourceBackedErrorV0::SourceChanged);
        }

        let after_hash = source.current_ordinary_file_token()?;
        source.revalidate_same_object()?;
        let after = source.current_ordinary_file_token()?;
        if before != after_hash || after_hash != after {
            continue;
        }
        return Ok(digest);
    }
    Err(CodexPromptHistorySourceBackedErrorV0::SourceChanged)
}

fn read_opened_prefix(
    source: &OpenedProviderSourceFile,
    target: u64,
    observed_len: u64,
) -> CodexPromptHistorySourceBackedResultV0<Option<[u8; 32]>> {
    if target > observed_len {
        return Ok(None);
    }
    let mut file = opened_file_from_start(source)?;
    let mut remaining = target;
    let mut digest = Sha256::new();
    let mut bytes = [0_u8; 64 * 1024];
    while remaining > 0 {
        let take = bytes
            .len()
            .min(usize::try_from(remaining).unwrap_or(usize::MAX));
        let count = file.read(&mut bytes[..take])?;
        if count == 0 {
            return Ok(None);
        }
        digest.update(&bytes[..count]);
        remaining = remaining.saturating_sub(
            u64::try_from(count)
                .map_err(|_| CodexPromptHistorySourceBackedErrorV0::CountMismatch)?,
        );
    }
    Ok(Some(digest.finalize().into()))
}

pub(super) fn verify_frozen_prefix(
    source: &OpenedProviderSourceFile,
    frozen_len: u64,
    expected_digest: [u8; 32],
) -> CodexPromptHistorySourceBackedResultV0<()> {
    let actual = hash_opened_prefix(source, frozen_len)?
        .ok_or(CodexPromptHistorySourceBackedErrorV0::SourceChanged)?;
    if actual != expected_digest {
        return Err(CodexPromptHistorySourceBackedErrorV0::SourceChanged);
    }
    Ok(())
}

pub(super) fn observation_wire(
    metadata: &Metadata,
    ordinary_file_token: [u8; 32],
    whole_source_digest: [u8; 32],
) -> CodexPromptHistorySourceBackedResultV0<ObservationWireV0> {
    let (modified_after_epoch, duration) = match metadata.modified()?.duration_since(UNIX_EPOCH) {
        Ok(duration) => (true, duration),
        Err(error) => (false, error.duration()),
    };
    Ok(ObservationWireV0 {
        length: metadata.len(),
        modified_after_epoch,
        modified_seconds: duration.as_secs(),
        modified_nanos: duration.subsec_nanos(),
        readonly: metadata.permissions().readonly(),
        ordinary_file_token,
        whole_source_digest,
    })
}

pub(super) fn stable_current_ordinary_file_observation(
    source: &OpenedProviderSourceFile,
) -> CodexPromptHistorySourceBackedResultV0<(Metadata, [u8; 32])> {
    let opened_token = source.ordinary_file_token();
    if source.current_ordinary_file_token()? == opened_token
        && source.revalidate().is_ok()
        && source.current_ordinary_file_token()? == opened_token
    {
        return Ok((source.metadata().clone(), opened_token));
    }

    for _ in 0..2 {
        source.revalidate_same_object()?;
        let before = source.current_ordinary_file_token()?;
        let metadata = source.file().metadata()?;
        let after_metadata = source.current_ordinary_file_token()?;
        source.revalidate_same_object()?;
        let after = source.current_ordinary_file_token()?;
        if before == after_metadata && after_metadata == after {
            return Ok((metadata, after));
        }
    }
    Err(CodexPromptHistorySourceBackedErrorV0::SourceChanged)
}

pub(super) fn exact_ordinary_file_observation_matches(
    metadata: &Metadata,
    ordinary_file_token: [u8; 32],
    expected: &CertifiedSource,
) -> CodexPromptHistorySourceBackedResultV0<bool> {
    if expected.parser_revision() != PARSER_REVISION
        || expected.observation().revision_kind() != SOURCE_REVISION_KIND
    {
        return Ok(false);
    }
    let expected_observation = decode_observation(expected)?;
    Ok(observation_wire(
        metadata,
        ordinary_file_token,
        expected_observation.whole_source_digest,
    )? == expected_observation)
}

fn decode_observation(
    certificate: &CertifiedSource,
) -> CodexPromptHistorySourceBackedResultV0<ObservationWireV0> {
    serde_json::from_slice(certificate.observation().revision())
        .map_err(|_| CodexPromptHistorySourceBackedErrorV0::InvalidCheckpoint)
}

pub(super) fn terminal_prefix(
    certificate: &CertifiedSource,
) -> CodexPromptHistorySourceBackedResultV0<(u64, [u8; 32])> {
    let observation = decode_observation(certificate)?;
    Ok((observation.length, observation.whole_source_digest))
}

pub(super) fn decode_checkpoint(
    certificate: &CertifiedSource,
) -> CodexPromptHistorySourceBackedResultV0<CheckpointV0> {
    if certificate.parser_revision() != PARSER_REVISION {
        return Err(CodexPromptHistorySourceBackedErrorV0::InvalidCheckpoint);
    }
    let frontier = certificate
        .frontier()
        .ok_or(CodexPromptHistorySourceBackedErrorV0::InvalidCheckpoint)?;
    if frontier.checkpoint_kind() != FRONTIER_KIND {
        return Err(CodexPromptHistorySourceBackedErrorV0::InvalidCheckpoint);
    }
    let TypedKey::Bytes(bytes) = frontier.checkpoint() else {
        return Err(CodexPromptHistorySourceBackedErrorV0::InvalidCheckpoint);
    };
    let checkpoint: CheckpointV0 = serde_json::from_slice(bytes)
        .map_err(|_| CodexPromptHistorySourceBackedErrorV0::InvalidCheckpoint)?;
    if checkpoint.version != CHECKPOINT_VERSION
        || checkpoint.certified_prefix_bytes != frontier.certified_prefix_bytes()
        || checkpoint.certified_prefix_bytes != certificate.counts().certified_bytes
        || checkpoint.complete_records != certificate.counts().complete_records
        || frontier.certified_prefix_digest() != certificate.content_digest()
    {
        return Err(CodexPromptHistorySourceBackedErrorV0::InvalidCheckpoint);
    }
    Ok(checkpoint)
}
