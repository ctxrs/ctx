use super::*;

pub(super) fn base_sources_for_root(
    adapter: &dyn JsonlFamilyAdapter,
    inventory: &JsonlFamilyInventory,
    sink: &SourceBackedGenerationSink<'_>,
) -> Result<Vec<CertifiedSource>> {
    let root = inventory
        .authority
        .as_ref()
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "present JSONL inventory has no retained root authority".to_owned(),
            )
        })?
        .named_path();
    source_backed_base_sources(sink, |source| adapter.owns(source))
        .into_iter()
        .filter_map(|source| match base_source_path(adapter, &source) {
            Ok(path) if path.starts_with(root) => Some(Ok(source)),
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .collect()
}

fn base_source_path(
    adapter: &dyn JsonlFamilyAdapter,
    certificate: &CertifiedSource,
) -> Result<PathBuf> {
    certificate.validate_contract().map_err(contract_error)?;
    if certificate.parser_revision() != adapter.parser_revision() {
        return Err(CaptureError::InvalidPayload(
            "JSONL base parser revision changed".to_owned(),
        ));
    }
    let frontier = certificate
        .frontier()
        .ok_or_else(|| CaptureError::InvalidPayload("JSONL base frontier is absent".to_owned()))?;
    if frontier.checkpoint_kind() != FAMILY_FRONTIER_KIND {
        return Err(CaptureError::InvalidPayload(
            "JSONL base frontier kind changed".to_owned(),
        ));
    }
    let TypedKey::Bytes(bytes) = frontier.checkpoint() else {
        return Err(CaptureError::InvalidPayload(
            "JSONL base checkpoint is malformed".to_owned(),
        ));
    };
    let checkpoint: FamilyCheckpoint = serde_json::from_slice(bytes)?;
    if checkpoint.physical.identity().source_descriptor_digest()
        != &certificate.observation().source().exact_descriptor_digest()
    {
        return Err(CaptureError::InvalidPayload(
            "JSONL base checkpoint source changed".to_owned(),
        ));
    }
    Ok(checkpoint.physical.identity().source_path().clone())
}
