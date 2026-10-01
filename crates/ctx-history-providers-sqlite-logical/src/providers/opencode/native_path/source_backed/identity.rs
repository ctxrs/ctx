use super::*;

pub(super) fn session_id(
    source: &SourceKey,
    native_identity: &str,
) -> OpenCodeSourceBackedResult<StableEntityId> {
    let native_session_key =
        NativeSessionKey::native_id(NATIVE_SESSION_NAMESPACE, TypedKey::utf8(native_identity)?)?;
    Ok(derive_session_id(SessionIdentityInput {
        source,
        // Preserve released message/part identities. Other representations must
        // have distinct compact IDs as well as distinct provenance descriptors.
        logical_session_kind: if source.schema_variant() == "opencode-family-message_part-v1" {
            LOGICAL_SESSION_KIND
        } else {
            source.schema_variant()
        },
        native_session_key: &native_session_key,
    })?)
}

pub(super) fn source_key_scoped(
    dialect: &OpenCodeSqliteDialect,
    family: OpenCodeNativeSchemaFamily,
    source_scope: SourceAnchorScope,
) -> OpenCodeSourceBackedResult<SourceKey> {
    let anchor = SourceAnchor::provider_native(
        format!("{}.sqlite-authority", dialect.provider.as_str()),
        TypedKey::utf8(SOURCE_ANCHOR_KEY)?,
    )?;
    Ok(SourceKey::derive_scoped(
        dialect.provider.as_str(),
        dialect.source_format,
        format!("opencode-family-{}-v1", family.label()),
        SOURCE_IDENTITY_VERSION,
        anchor,
        source_scope,
    )?)
}

pub(super) fn schema_family_for_source(
    dialect: &OpenCodeSqliteDialect,
    source: &SourceKey,
    source_scope: SourceAnchorScope,
) -> Option<OpenCodeNativeSchemaFamily> {
    [
        OpenCodeNativeSchemaFamily::SessionMessageSeq,
        OpenCodeNativeSchemaFamily::SessionMessageSynthesizedSeq,
        OpenCodeNativeSchemaFamily::SessionEntry,
        OpenCodeNativeSchemaFamily::LegacyMessage,
        OpenCodeNativeSchemaFamily::MessagePart,
    ]
    .into_iter()
    .find(|family| {
        source_key_scoped(dialect, *family, source_scope)
            .is_ok_and(|candidate| candidate.exact_descriptor_eq(source))
    })
}
