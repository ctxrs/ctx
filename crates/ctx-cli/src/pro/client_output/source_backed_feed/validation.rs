use super::*;

pub(super) fn validate_consumer_state(
    manifest: &SourceBackedProManifest,
    state: &SourceManifestBegan,
) -> Result<()> {
    state
        .validate_contents()
        .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
    if state.core_generation_id != manifest.core_generation_id {
        bail!("invalid_response: Pro began the wrong source manifest");
    }
    Ok(())
}

pub(super) fn validate_prepared_source(
    request: &PrepareSourceRequest,
    prepared: &SourcePrepared,
) -> Result<()> {
    prepared
        .validate()
        .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
    let progress = &prepared.progress;
    if prepared.core_generation_id != request.core_generation_id
        || !progress.source.exact_descriptor_eq(&request.source)
        || progress.certified_revision_sha256 != request.certified_revision_sha256
        || progress.materializer_revision != request.materializer_revision
    {
        bail!("invalid_response: Pro prepared the wrong canonical source");
    }
    match (request.disposition, request.expected_prior.as_ref()) {
        (SourceBackedProDisposition::NewSource, None) => {
            if progress.source_epoch != 1 || progress.frontier.is_some() || progress.terminal {
                bail!("invalid_response: new Pro source did not start from genesis");
            }
        }
        (SourceBackedProDisposition::Resume, Some(prior)) => {
            if !progress.exact_eq(prior) {
                bail!("invalid_response: Pro did not resume the exact source CAS");
            }
        }
        (SourceBackedProDisposition::Rewrite, Some(prior)) => {
            if progress.source_epoch
                != prior
                    .source_epoch
                    .checked_add(1)
                    .ok_or_else(|| anyhow!("invalid_request: source epoch is exhausted"))?
                || progress.frontier.is_some()
                || progress.terminal
            {
                bail!("invalid_response: Pro did not invalidate the rewritten source epoch");
            }
        }
        _ => bail!("invalid_request: source disposition and prior progress disagree"),
    }
    Ok(())
}

pub(super) fn validate_provider_page(
    source: &CertifiedSource,
    prepared: &SourceBackedProProgress,
    page: &SourceBackedProviderPage,
) -> Result<()> {
    let source_key = source.observation().source();
    if !page.source.exact_descriptor_eq(source_key)
        || page.expected_prior_frontier != prepared.frontier
    {
        bail!("source_changed: provider returned a page for the wrong source frontier");
    }
    if page.terminal {
        if page.next_frontier.as_ref() != source.frontier() {
            bail!("source_changed: terminal provider page missed the certified frontier");
        }
    } else if page.records.is_empty()
        || page.next_frontier.is_none()
        || page.next_frontier == page.expected_prior_frontier
        || page.next_frontier.as_ref() == source.frontier()
    {
        bail!("source_changed: provider returned a non-progressing source page");
    }
    Ok(())
}

pub(super) fn validate_source_backed_receipt(
    manifest: &SourceBackedProManifest,
    materializer_revision: &str,
    expected_sources: &[SourceBackedProProgress],
    receipt: &SourceBackedProReceipt,
) -> Result<()> {
    let expected_progress = SourceProgressReceipt::from_progress(expected_sources)
        .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
    if receipt.core_generation_id != manifest.core_generation_id
        || receipt.materializer_revision != materializer_revision
        || receipt.progress != expected_progress
    {
        bail!("invalid_response: Pro published the wrong source-backed receipt");
    }
    Ok(())
}

pub(super) fn source_identity_digest(source: &CertifiedSource) -> [u8; 32] {
    source.observation().source().identity().digest()
}

pub(super) fn validate_request<T>(request: &T) -> Result<()>
where
    T: SourceRequestValidation,
{
    request
        .validate_request()
        .map_err(|error| anyhow!("invalid_request: {}", error.message))
}

pub(super) fn validate_response<T>(response: &T) -> Result<()>
where
    T: SourceResponseValidation,
{
    response
        .validate_response()
        .map_err(|error| anyhow!("invalid_response: {}", error.message))
}

pub(super) trait SourceRequestValidation {
    fn validate_request(&self) -> std::result::Result<(), ctx_pro_host_protocol::ProtocolError>;
}

impl SourceRequestValidation for PrepareSourceRequest {
    fn validate_request(&self) -> std::result::Result<(), ctx_pro_host_protocol::ProtocolError> {
        self.validate()
    }
}

impl SourceRequestValidation for DeleteSourceRequest {
    fn validate_request(&self) -> std::result::Result<(), ctx_pro_host_protocol::ProtocolError> {
        self.validate()
    }
}

impl SourceRequestValidation for FinishSourceManifestRequest {
    fn validate_request(&self) -> std::result::Result<(), ctx_pro_host_protocol::ProtocolError> {
        self.validate()
    }
}

pub(super) trait SourceResponseValidation {
    fn validate_response(&self) -> std::result::Result<(), ctx_pro_host_protocol::ProtocolError>;
}

macro_rules! source_response_validation {
    ($($type:ty),+ $(,)?) => {
        $(
            impl SourceResponseValidation for $type {
                fn validate_response(
                    &self,
                ) -> std::result::Result<(), ctx_pro_host_protocol::ProtocolError> {
                    self.validate()
                }
            }
        )+
    };
}

source_response_validation!(
    SourceManifestBegan,
    SourcePrepared,
    SourcePagesMaterialized,
    SourceDeleted,
    SourceManifestFinished,
);
