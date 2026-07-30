use std::collections::BTreeMap;

use super::super::super::workspace::trae_workspace_id;
use super::super::scanner::{validate_schema, TraeSqliteDatabase};
use super::*;

const TRAE_HYDRATION_NATIVE_KEY_BATCH: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TraeHydratedRecordV0 {
    pub(crate) exact_text: String,
}

#[derive(Debug, Clone)]
pub(crate) struct TraeLocatorResolverV0 {
    path: PathBuf,
}

impl TraeLocatorResolverV0 {
    pub(crate) fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub(crate) fn hydrate(
        &self,
        locator: &SourceRecordLocator,
    ) -> TraeSourceBackedResultV0<TraeHydratedRecordV0> {
        self.hydrate_locators(&[locator])?
            .pop()
            .ok_or(TraeSourceBackedErrorV0::LocatorMessageMissing)
    }

    pub(crate) fn hydrate_locators(
        &self,
        locators: &[&SourceRecordLocator],
    ) -> TraeSourceBackedResultV0<Vec<TraeHydratedRecordV0>> {
        let canonical_path = explicit_trae_leaf(&self.path)?;
        let source = source_key_for_workspace(&trae_workspace_id(&canonical_path))?;
        let mut coordinates = Vec::with_capacity(locators.len());
        for locator in locators {
            locator.validate_contract()?;
            if !source.exact_descriptor_eq(locator.source()) {
                return Err(TraeSourceBackedErrorV0::LocatorSourceMismatch);
            }
            if locator.revision_policy() != LocatorRevisionPolicy::StableRecordEvidence
                || locator.certified_source_revision_digest().is_some()
            {
                return Err(TraeSourceBackedErrorV0::SourceRevisionMismatch);
            }
            coordinates.push(decode_locator(locator)?);
        }
        let (_, values) = TraeSqliteDatabase::open(&canonical_path, |conn| {
            validate_schema(conn, &canonical_path)?;
            let mut values = BTreeMap::new();
            let mut keys = coordinates
                .iter()
                .map(|coordinate| coordinate.chat_key.as_str())
                .collect::<Vec<_>>();
            keys.sort_unstable();
            keys.dedup();
            for chunk in keys.chunks(TRAE_HYDRATION_NATIVE_KEY_BATCH) {
                for chat_key in chunk {
                    let key_index = TRAE_CHAT_KEYS
                        .iter()
                        .position(|candidate| candidate == chat_key)
                        .and_then(|index| u16::try_from(index).ok())
                        .ok_or_else(|| {
                            CaptureError::InvalidPayload(
                                "Trae locator has an unsupported ItemTable key".to_owned(),
                            )
                        })?;
                    let value = super::super::super::trae_complete_value(conn, key_index)?
                        .ok_or_else(|| {
                            CaptureError::InvalidPayload(
                                "Trae locator ItemTable value is unavailable".to_owned(),
                            )
                        })?;
                    values.insert((*chat_key).to_owned(), (key_index, value));
                }
            }
            Ok(values)
        })?;
        let mut hydrated = Vec::with_capacity(locators.len());
        for (locator, coordinate) in locators.iter().zip(coordinates) {
            let (key_index, value) = values
                .get(&coordinate.chat_key)
                .ok_or(TraeSourceBackedErrorV0::LocatorValueMissing)?;
            let actual_digest: [u8; 32] = Sha256::digest(value).into();
            if actual_digest != coordinate.value_digest || &actual_digest != locator.record_digest()
            {
                return Err(TraeSourceBackedErrorV0::LocatorValueDigestMismatch);
            }
            let (_, exact_text) = super::super::super::trae_complete_message(
                value,
                *key_index,
                coordinate.session_index,
                coordinate.message_index,
                &coordinate.provider_session_id,
            )?
            .ok_or(TraeSourceBackedErrorV0::LocatorMessageMissing)?;
            hydrated.push(TraeHydratedRecordV0 { exact_text });
        }
        Ok(hydrated)
    }
}

pub(crate) fn hydrate_trae_source_backed_locator_v0(
    path: &Path,
    locator: &SourceRecordLocator,
) -> TraeSourceBackedResultV0<TraeHydratedRecordV0> {
    TraeLocatorResolverV0::new(path).hydrate(locator)
}

struct DecodedLocator {
    chat_key: String,
    value_digest: [u8; 32],
    session_index: u32,
    message_index: u32,
    provider_session_id: String,
}

fn decode_locator(locator: &SourceRecordLocator) -> TraeSourceBackedResultV0<DecodedLocator> {
    if locator.source().provider() != CaptureProvider::Trae.as_str()
        || locator.source().source_format() != TRAE_STATE_VSCDB_SOURCE_FORMAT
        || locator.source().schema_variant() != TRAE_SOURCE_SCHEMA_VARIANT
        || locator.source().provider_identity_version() != 1
    {
        return Err(TraeSourceBackedErrorV0::InvalidLocator);
    }
    let NativeRecordCoordinate::ProviderSqlite {
        logical_relation,
        primary_key,
        row_version,
    } = locator.coordinate()
    else {
        return Err(TraeSourceBackedErrorV0::InvalidLocator);
    };
    let TypedKey::Composite(parts) = primary_key else {
        return Err(TraeSourceBackedErrorV0::InvalidLocator);
    };
    let [TypedKey::Utf8(chat_key), TypedKey::U64(session_index), TypedKey::U64(message_index), TypedKey::Utf8(provider_session_id)] =
        parts.as_slice()
    else {
        return Err(TraeSourceBackedErrorV0::InvalidLocator);
    };
    let Some(TypedKey::Bytes(value_digest)) = row_version else {
        return Err(TraeSourceBackedErrorV0::InvalidLocator);
    };
    if logical_relation != TRAE_LOCATOR_RELATION
        || value_digest.len() != 32
        || !TRAE_CHAT_KEYS.contains(&chat_key.as_str())
    {
        return Err(TraeSourceBackedErrorV0::InvalidLocator);
    }
    let mut value_digest_bytes = [0_u8; 32];
    value_digest_bytes.copy_from_slice(value_digest);
    if locator.record_digest() != &value_digest_bytes {
        return Err(TraeSourceBackedErrorV0::InvalidLocator);
    }
    Ok(DecodedLocator {
        chat_key: chat_key.clone(),
        value_digest: value_digest_bytes,
        session_index: u32::try_from(*session_index)
            .map_err(|_| TraeSourceBackedErrorV0::InvalidLocator)?,
        message_index: u32::try_from(*message_index)
            .map_err(|_| TraeSourceBackedErrorV0::InvalidLocator)?,
        provider_session_id: provider_session_id.clone(),
    })
}
