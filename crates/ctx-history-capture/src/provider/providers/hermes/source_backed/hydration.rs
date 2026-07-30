use super::*;

const HERMES_HYDRATION_NATIVE_KEY_BATCH: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HermesHydratedMessage {
    pub(crate) provider_bytes: Vec<u8>,
    pub(crate) text: String,
    pub(crate) provider_session_id: String,
    pub(crate) provider_event_hash: String,
    pub(crate) normalized_payload_hash: String,
}

#[derive(Debug, Clone)]
pub(crate) struct HermesLocatorResolver {
    path: PathBuf,
    source: SourceKey,
}

impl HermesLocatorResolver {
    pub(crate) fn new(path: impl Into<PathBuf>, source: SourceKey) -> Self {
        Self {
            path: path.into(),
            source,
        }
    }

    pub(crate) fn hydrate(
        &self,
        locator: &SourceRecordLocator,
    ) -> HermesSourceBackedResult<HermesHydratedMessage> {
        self.hydrate_locators(&[locator])?
            .pop()
            .ok_or(HermesSourceBackedError::MissingRecord)
    }

    pub(crate) fn hydrate_locators(
        &self,
        locators: &[&SourceRecordLocator],
    ) -> HermesSourceBackedResult<Vec<HermesHydratedMessage>> {
        let mut coordinates = Vec::with_capacity(locators.len());
        for locator in locators {
            locator.validate_contract()?;
            if !self.source.exact_descriptor_eq(locator.source())
                || locator.revision_policy() != LocatorRevisionPolicy::StableRecordEvidence
                || locator.certified_source_revision_digest().is_some()
            {
                return Err(HermesSourceBackedError::InvalidLocator);
            }
            coordinates.push(decode_message_coordinate(locator)?);
        }
        let (source_root, sqlite_snapshot) = open_root_authorized_snapshot(&self.path)?;
        let opening_evidence = sqlite_snapshot.evidence().clone();
        let conn = sqlite_snapshot.connection()?;
        let operation = (|| {
            let schema = HermesSchema::detect(conn)?;
            let mut hydrated = Vec::with_capacity(locators.len());
            for chunk in coordinates.chunks(HERMES_HYDRATION_NATIVE_KEY_BATCH) {
                for (provider_session_id, message_id, row_version) in chunk {
                    let rowid =
                        find_message_rowid(conn, &schema, provider_session_id, *message_id)?
                            .ok_or(HermesSourceBackedError::MissingRecord)?;
                    let values = match load_hermes_message_values(conn, rowid) {
                        Ok(values) => values,
                        Err(CaptureError::Sqlite(rusqlite::Error::QueryReturnedNoRows)) => {
                            return Err(HermesSourceBackedError::MissingRecord);
                        }
                        Err(error) => return Err(error.into()),
                    };
                    let (actual_session_id, provider_event_hash, normalized_payload_hash, text) =
                        hermes_complete_message_with_normalized_hash(conn, &values)?;
                    if actual_session_id != *provider_session_id
                        || provider_event_hash != format!("message:{message_id}")
                    {
                        return Err(HermesSourceBackedError::StaleRecordEvidence);
                    }
                    let record_digest = decode_sha256(hermes_record_digest(&values).as_str())?;
                    if &record_digest != row_version {
                        return Err(HermesSourceBackedError::StaleRecordEvidence);
                    }
                    hydrated.push(HermesHydratedMessage {
                        provider_bytes: text.as_bytes().to_vec(),
                        text,
                        provider_session_id: actual_session_id,
                        provider_event_hash,
                        normalized_payload_hash,
                    });
                }
            }
            Ok(hydrated)
        })();
        let finish = sqlite_snapshot.finish();
        let hydrated = operation?;
        let closing_evidence = finish?;
        if closing_evidence != opening_evidence {
            return Err(HermesSourceBackedError::StaleSourceEvidence);
        }
        source_root.revalidate()?;
        Ok(hydrated)
    }
}

pub(crate) fn hydrate_hermes_source_backed_message(
    path: &Path,
    locator: &SourceRecordLocator,
) -> HermesSourceBackedResult<HermesHydratedMessage> {
    HermesLocatorResolver::new(path, locator.source().clone()).hydrate(locator)
}

fn find_message_rowid(
    conn: &rusqlite::Connection,
    schema: &HermesSchema,
    provider_session_id: &str,
    message_id: i64,
) -> HermesSourceBackedResult<Option<i64>> {
    let visibility = schema.message_visibility();
    let visibility = if visibility.is_empty() {
        String::new()
    } else {
        format!(" and {visibility}")
    };
    let sql = format!(
        "select m.rowid from messages m \
         where m.session_id = ?1 collate binary and m.id = ?2{visibility} limit 2"
    );
    let mut statement = conn.prepare(&sql).map_err(CaptureError::from)?;
    let mut rows = statement
        .query(rusqlite::params![provider_session_id, message_id])
        .map_err(CaptureError::from)?;
    let Some(first) = rows.next().map_err(CaptureError::from)? else {
        return Ok(None);
    };
    let rowid = first.get(0).map_err(CaptureError::from)?;
    if rows.next().map_err(CaptureError::from)?.is_some() {
        return Err(HermesSourceBackedError::StaleRecordEvidence);
    }
    Ok(Some(rowid))
}

fn decode_message_coordinate(
    locator: &SourceRecordLocator,
) -> HermesSourceBackedResult<(String, i64, [u8; 32])> {
    let NativeRecordCoordinate::ProviderSqlite {
        logical_relation,
        primary_key,
        row_version,
    } = locator.coordinate()
    else {
        return Err(HermesSourceBackedError::InvalidLocator);
    };
    let TypedKey::Composite(parts) = primary_key else {
        return Err(HermesSourceBackedError::InvalidLocator);
    };
    let [TypedKey::Utf8(provider_session_id), TypedKey::I64(message_id)] = parts.as_slice() else {
        return Err(HermesSourceBackedError::InvalidLocator);
    };
    let Some(TypedKey::Bytes(row_version)) = row_version else {
        return Err(HermesSourceBackedError::InvalidLocator);
    };
    if logical_relation != HERMES_MESSAGE_RELATION {
        return Err(HermesSourceBackedError::InvalidLocator);
    }
    let row_version = row_version
        .as_slice()
        .try_into()
        .map_err(|_| HermesSourceBackedError::InvalidLocator)?;
    if locator.record_digest() != &row_version {
        return Err(HermesSourceBackedError::InvalidLocator);
    }
    Ok((provider_session_id.clone(), *message_id, row_version))
}
