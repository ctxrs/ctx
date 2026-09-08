use super::{DirectJsonlEvent, DirectJsonlRetryDiscriminator, ProjectionContractError, TypedKey};

// These constructors validate only provider-origin key material. Source binding,
// fallback occurrence lookup, and storage errors remain at their fatal boundaries.
pub(super) fn validate_provider_event_key(
    event: &DirectJsonlEvent,
) -> Result<(), ProjectionContractError> {
    let key = TypedKey::utf8(
        event
            .native_record_id
            .as_deref()
            .unwrap_or(&event.provider_event_hash),
    )?;
    native_event_key(event, key)?;
    Ok(())
}

pub(super) fn native_event_key(
    event: &DirectJsonlEvent,
    native_record_key: TypedKey,
) -> Result<TypedKey, ProjectionContractError> {
    let native_subrecord_key = match &event.stable_retry_discriminator {
        Some(DirectJsonlRetryDiscriminator::FactoryDroidToolResult { tool_use_id }) => {
            TypedKey::composite(vec![
                TypedKey::utf8("factory-ai-droid.retry-tool-result")?,
                TypedKey::utf8(tool_use_id)?,
            ])?
        }
        None => TypedKey::U64(u64::from(event.sub_ordinal)),
    };
    TypedKey::composite(vec![native_record_key, native_subrecord_key])
}
