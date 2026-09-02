use super::*;

pub(super) fn validate_strict_publication_metadata_ownership(
    job: &Value,
    metadata: &SourceBackedPublicationMetadata,
) -> Result<()> {
    let request_id = required_nonempty_string(job, "request_id", "source refresh job")?;
    if metadata.request_id != request_id {
        bail!("active Core refresh metadata belongs to a different request");
    }
    let operation = SourceBackedRefreshOperation::from_request_json(job)?;
    if metadata.operation != operation {
        bail!("active Core refresh metadata has a different operation");
    }
    let scope = refresh_scope_from_json(job.get("refresh_scope"))?;
    if metadata.refresh_scope != scope {
        bail!("active Core refresh metadata has a different scope");
    }
    Ok(())
}
