use ctx_history_capture_runtime::SourceBackedRouteResult;
use ctx_history_core::{CertifiedSource, ScannedSourceCounts, SourceFrontier};

use super::super::{
    contract_error, route_invalid, source_observation, FamilyCheckpoint, JsonlFamilyAdapter,
    JsonlFamilyAppendTrustContract, JsonlFamilyError, JsonlFamilyLeaf, JsonlFamilyRuntime,
    JsonlFamilyTerminalProof, JsonlResult, JsonlRuntimeError, FAMILY_FRONTIER_KIND,
};

pub(super) fn fit_semantic_provider_checkpoint<R: JsonlFamilyRuntime>(
    adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
    mut checkpoint: FamilyCheckpoint,
) -> JsonlResult<FamilyCheckpoint, JsonlRuntimeError<R>> {
    while !checkpoint.fits_frontier_key::<JsonlRuntimeError<R>>()? {
        let provider_checkpoint = checkpoint.provider_checkpoint.as_ref().ok_or_else(|| {
            JsonlRuntimeError::<R>::invalid_payload(
                "JSONL family checkpoint exceeds the SourceFrontier bound without provider state"
                    .to_owned(),
            )
        })?;
        checkpoint.provider_checkpoint =
            adapter.shed_optional_provider_checkpoint_evidence(provider_checkpoint)?;
    }
    Ok(checkpoint)
}

pub(super) fn terminal_proof_for_checkpoint<R: JsonlFamilyRuntime>(
    adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
    leaf: &JsonlFamilyLeaf<JsonlRuntimeError<R>>,
    certificate: &CertifiedSource,
    checkpoint: &FamilyCheckpoint,
    append_only_trust_allowed: bool,
) -> JsonlResult<JsonlFamilyTerminalProof<JsonlRuntimeError<R>>, JsonlRuntimeError<R>> {
    let retained = checkpoint.physical.source_observation();
    let force_authentication = retained.differs_only_by_change_identity(leaf.observation());
    if leaf.logical_eof().is_some() {
        let admitted_eof_sha256 = checkpoint
            .exact_admitted_eof_sha256()
            .ok_or_else(JsonlRuntimeError::<R>::source_changed)?;
        JsonlFamilyTerminalProof::forced_frozen_prefix_with_hash(
            adapter,
            leaf,
            certificate,
            checkpoint.physical.admitted_length(),
            admitted_eof_sha256,
            super::super::terminal::JsonlFamilyTerminalPrefixHash::Sha256,
        )
    } else if force_authentication {
        if let Some(admitted_eof_sha256) = checkpoint.exact_admitted_eof_sha256() {
            JsonlFamilyTerminalProof::forced_frozen_prefix_with_hash(
                adapter,
                leaf,
                certificate,
                retained.length(),
                admitted_eof_sha256,
                super::super::terminal::JsonlFamilyTerminalPrefixHash::Sha256,
            )
        } else if checkpoint.physical.complete_prefix_end() == retained.length() {
            JsonlFamilyTerminalProof::forced_frozen_prefix_with_hash(
                adapter,
                leaf,
                certificate,
                retained.length(),
                *checkpoint.physical.complete_prefix_sha256(),
                super::super::terminal::JsonlFamilyTerminalPrefixHash::SharedJsonlDomain,
            )
        } else {
            Err(JsonlRuntimeError::<R>::source_changed())
        }
    } else if leaf.whole_record || !adapter.append_mode().certified_suffix() {
        if let Some(admitted_eof_sha256) = checkpoint.exact_admitted_eof_sha256() {
            JsonlFamilyTerminalProof::frozen_prefix(
                adapter,
                leaf,
                certificate,
                retained.length(),
                admitted_eof_sha256,
            )
        } else if checkpoint.physical.complete_prefix_end() == retained.length() {
            JsonlFamilyTerminalProof::frozen_shared_prefix(
                adapter,
                leaf,
                certificate,
                retained.length(),
                *checkpoint.physical.complete_prefix_sha256(),
            )
        } else {
            JsonlFamilyTerminalProof::exact_file(adapter, leaf, certificate)
        }
    } else if append_only_trust_allowed
        && adapter.append_trust_contract() == JsonlFamilyAppendTrustContract::AppendOnlySameObjectV1
        && adapter.allows_direct_append_for_leaf(leaf)
    {
        JsonlFamilyTerminalProof::append_only_same_object_v1(
            adapter,
            leaf,
            certificate,
            checkpoint.physical.complete_prefix_end(),
            checkpoint.exact_admitted_eof_sha256(),
        )
    } else if let Some(admitted_eof_sha256) = checkpoint.exact_admitted_eof_sha256() {
        JsonlFamilyTerminalProof::frozen_prefix(
            adapter,
            leaf,
            certificate,
            checkpoint.physical.source_observation().length(),
            admitted_eof_sha256,
        )
    } else {
        JsonlFamilyTerminalProof::frozen_shared_prefix(
            adapter,
            leaf,
            certificate,
            checkpoint.physical.complete_prefix_end(),
            *checkpoint.physical.complete_prefix_sha256(),
        )
    }
}

pub(super) fn certify<R: JsonlFamilyRuntime>(
    adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
    leaf: &JsonlFamilyLeaf<JsonlRuntimeError<R>>,
    checkpoint: FamilyCheckpoint,
) -> SourceBackedRouteResult<CertifiedSource> {
    if !checkpoint.valid_for(adapter, leaf)
        || checkpoint.physical.logical_eof() != leaf.logical_eof()
    {
        return Err(route_invalid("JSONL checkpoint is internally inconsistent"));
    }
    let complete_records = checkpoint.logical_complete_records;
    let rejected_records = checkpoint.rejected_logical_records;
    let ignored = complete_records
        .checked_sub(
            checkpoint
                .indexed_documents
                .checked_add(rejected_records)
                .ok_or_else(|| route_invalid("JSONL logical count overflowed"))?,
        )
        .ok_or_else(|| route_invalid("JSONL logical ignored count underflowed"))?;
    let frontier = SourceFrontier::new(
        FAMILY_FRONTIER_KIND,
        checkpoint
            .encode_frontier_key::<JsonlRuntimeError<R>>()
            .map_err(route_invalid)?,
        checkpoint.physical.complete_prefix_end(),
        *checkpoint.physical.complete_prefix_sha256(),
    )
    .map_err(route_invalid)?;
    CertifiedSource::certify_with_frontier(
        source_observation::<JsonlRuntimeError<R>>(&leaf.source, &leaf.observation)
            .map_err(route_invalid)?,
        source_observation::<JsonlRuntimeError<R>>(&leaf.source, &leaf.observation)
            .map_err(route_invalid)?,
        adapter.parser_revision(),
        *checkpoint.physical.complete_prefix_sha256(),
        ScannedSourceCounts {
            complete_records,
            retained_records: checkpoint.indexed_documents,
            rejected_records,
            ignored_records: ignored,
            indexed_documents: checkpoint.indexed_documents,
            certified_bytes: checkpoint.physical.complete_prefix_end(),
        },
        Some(frontier),
    )
    .map_err(route_invalid)
}

pub(in crate::family::route) fn decode_checkpoint<R: JsonlFamilyRuntime>(
    adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
    leaf: &JsonlFamilyLeaf<JsonlRuntimeError<R>>,
    certificate: &CertifiedSource,
) -> JsonlResult<FamilyCheckpoint, JsonlRuntimeError<R>> {
    certificate
        .validate_contract()
        .map_err(contract_error::<JsonlRuntimeError<R>>)?;
    leaf.source
        .validate_exact_descriptor(certificate.observation().source())
        .map_err(contract_error::<JsonlRuntimeError<R>>)?;
    if certificate.parser_revision() != adapter.parser_revision() {
        return Err(JsonlRuntimeError::<R>::invalid_payload(
            "JSONL base parser revision changed".to_owned(),
        ));
    }
    let frontier = certificate.frontier().ok_or_else(|| {
        JsonlRuntimeError::<R>::invalid_payload("JSONL base frontier is absent".to_owned())
    })?;
    if frontier.checkpoint_kind() != FAMILY_FRONTIER_KIND {
        return Err(JsonlRuntimeError::<R>::invalid_payload(
            "JSONL base frontier kind changed".to_owned(),
        ));
    }
    let checkpoint =
        FamilyCheckpoint::decode_frontier_key::<JsonlRuntimeError<R>>(frontier.checkpoint())?;
    let classified = checkpoint
        .indexed_documents
        .checked_add(checkpoint.rejected_logical_records)
        .ok_or_else(|| {
            JsonlRuntimeError::<R>::invalid_payload("JSONL base counts are invalid".to_owned())
        })?;
    let ignored = checkpoint
        .logical_complete_records
        .checked_sub(classified)
        .ok_or_else(|| {
            JsonlRuntimeError::<R>::invalid_payload("JSONL base counts are invalid".to_owned())
        })?;
    let counts = certificate.counts();
    if !checkpoint.valid_for(adapter, leaf)
        || checkpoint.physical.complete_prefix_end() != frontier.certified_prefix_bytes()
        || checkpoint.physical.complete_prefix_sha256() != frontier.certified_prefix_digest()
        || checkpoint.physical.complete_prefix_sha256() != certificate.content_digest()
        || checkpoint.indexed_documents != counts.retained_records
        || checkpoint.indexed_documents != counts.indexed_documents
        || checkpoint.rejected_logical_records != counts.rejected_records
        || ignored != counts.ignored_records
        || checkpoint.logical_complete_records != counts.complete_records
        || checkpoint.physical.complete_prefix_end() != counts.certified_bytes
        || certificate.observation()
            != &source_observation::<JsonlRuntimeError<R>>(
                &leaf.source,
                checkpoint.physical.source_observation(),
            )?
    {
        return Err(JsonlRuntimeError::<R>::invalid_payload(
            "JSONL base checkpoint does not reconcile".to_owned(),
        ));
    }
    Ok(checkpoint)
}

#[cfg(any(test, feature = "test-support"))]
pub fn checkpoint_admitted_revision_for_test(
    certificate: &CertifiedSource,
) -> JsonlResult<(Option<[u8; 32]>, bool), ctx_history_source_io::SourceIoError> {
    certificate
        .validate_contract()
        .map_err(contract_error::<ctx_history_source_io::SourceIoError>)?;
    let frontier = certificate.frontier().ok_or_else(|| {
        ctx_history_source_io::SourceIoError::InvalidPayload(
            "JSONL test certificate has no frontier".to_owned(),
        )
    })?;
    if frontier.checkpoint_kind() != FAMILY_FRONTIER_KIND {
        return Err(ctx_history_source_io::SourceIoError::InvalidPayload(
            "JSONL test certificate has the wrong frontier kind".to_owned(),
        ));
    }
    let checkpoint = FamilyCheckpoint::decode_frontier_key::<ctx_history_source_io::SourceIoError>(
        frontier.checkpoint(),
    )?;
    Ok((
        checkpoint.admitted_eof_sha256,
        checkpoint.complete_prefix_ends_with_terminal_nul_padding,
    ))
}
