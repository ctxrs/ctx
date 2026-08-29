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

pub(super) fn validate_physical_publication_metadata(
    metadata: &SourceBackedPublicationMetadata,
    verified: &VerifiedIndex,
    status_receipt: Option<&SourceBackedRefreshReceipt>,
) -> Result<SourceBackedRefreshReceipt> {
    let receipt = published_refresh_receipt_for_index(&metadata.response_value(), verified)
        .context("decode exact Core publication receipt")?;
    if receipt.published_generation != verified.generation_id() {
        bail!("active Core refresh metadata names a different generation");
    }
    if let Some(publication_receipt) = status_receipt {
        if receipt != *publication_receipt {
            bail!("active Core refresh metadata has a different terminal receipt");
        }
    }
    Ok(receipt)
}

pub(super) fn decode_published_request_receipt<Decode>(
    job: &Value,
    publication_receipt: &SourceBackedRefreshReceipt,
    decode: Decode,
) -> Result<SourceBackedRefreshReceipt>
where
    Decode: FnOnce(&Value) -> Result<SourceBackedRefreshReceipt>,
{
    let request_receipt = match job.get("request_outcome") {
        Some(outcome) => {
            let mut response = job.clone();
            response["receipt"] = outcome.clone();
            let receipt =
                decode(&response).context("recover exact logical source refresh outcome")?;
            if receipt == *publication_receipt {
                bail!("durable logical source refresh redundantly stores its publication receipt");
            }
            if receipt.previous_generation.as_deref()
                != Some(publication_receipt.published_generation.as_str())
                || receipt.published_generation != publication_receipt.published_generation
                || receipt.generation_changed
            {
                bail!("durable logical source refresh outcome is not an exact publication no-op");
            }
            receipt
        }
        None => publication_receipt.clone(),
    };
    validate_terminal_receipt_fields(job, &request_receipt)?;
    Ok(request_receipt)
}

fn validate_terminal_receipt_fields(
    job: &Value,
    receipt: &SourceBackedRefreshReceipt,
) -> Result<()> {
    if optional_generation(job.get("previous_generation"))? != receipt.previous_generation
        || required_generation(
            job.get("published_generation"),
            "durable terminal published generation",
        )? != receipt.published_generation
        || job.get("generation_changed").and_then(Value::as_bool)
            != Some(receipt.generation_changed)
        || job.get("outcome").and_then(Value::as_str) != Some(receipt.terminal_outcome())
    {
        bail!("durable logical source refresh response does not match its exact outcome receipt");
    }
    Ok(())
}
