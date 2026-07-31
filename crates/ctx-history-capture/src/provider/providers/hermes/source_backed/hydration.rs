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
    data_root: PathBuf,
    path: PathBuf,
    source: SourceKey,
    #[cfg(test)]
    snapshot_opens: std::cell::Cell<u64>,
    #[cfg(test)]
    native_key_batches: std::cell::Cell<u64>,
    #[cfg(test)]
    native_rows_read: std::cell::Cell<u64>,
}

impl HermesLocatorResolver {
    pub(crate) fn new(
        data_root: impl Into<PathBuf>,
        path: impl Into<PathBuf>,
        source: SourceKey,
    ) -> Self {
        Self {
            data_root: data_root.into(),
            path: path.into(),
            source,
            #[cfg(test)]
            snapshot_opens: std::cell::Cell::new(0),
            #[cfg(test)]
            native_key_batches: std::cell::Cell::new(0),
            #[cfg(test)]
            native_rows_read: std::cell::Cell::new(0),
        }
    }

    #[cfg(test)]
    pub(crate) fn counters(&self) -> (u64, u64, u64) {
        (
            self.snapshot_opens.get(),
            self.native_key_batches.get(),
            self.native_rows_read.get(),
        )
    }

    #[cfg(test)]
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
        let (_, sqlite_snapshot) = open_root_authorized_snapshot(&self.data_root, &self.path)?;
        #[cfg(test)]
        self.snapshot_opens
            .set(self.snapshot_opens.get().saturating_add(1));
        let opening_evidence = sqlite_snapshot.evidence().clone();
        let conn = sqlite_snapshot.connection()?;
        let operation = (|| {
            let schema = HermesSchema::detect(conn)?;
            let mut hydrated = Vec::with_capacity(locators.len());
            for chunk in coordinates.chunks(HERMES_HYDRATION_NATIVE_KEY_BATCH) {
                #[cfg(test)]
                self.native_key_batches
                    .set(self.native_key_batches.get().saturating_add(1));
                let rows = load_message_batch(conn, &schema, chunk)?;
                #[cfg(test)]
                self.native_rows_read.set(
                    self.native_rows_read
                        .get()
                        .saturating_add(rows.len() as u64),
                );
                for (provider_session_id, message_id, row_version) in chunk {
                    let values = rows
                        .get(&(provider_session_id.clone(), *message_id))
                        .ok_or(HermesSourceBackedError::MissingRecord)?;
                    let hermes_values = values
                        .iter()
                        .map(super::super::hermes_sqlite_value)
                        .collect::<Result<Vec<_>, _>>()?;
                    let row = super::super::layout::decode_hermes_message(&schema, &hermes_values)?;
                    if row.session_id != *provider_session_id || row.id != *message_id {
                        return Err(HermesSourceBackedError::StaleRecordEvidence);
                    }
                    let content = super::super::hermes_decode_content(row.content.as_deref());
                    let text = crate::provider::normalization::provider_value_text(&content)
                        .unwrap_or_else(|| {
                            row.tool_name
                                .as_ref()
                                .map(|name| format!("tool: {name}"))
                                .unwrap_or_else(|| format!("Hermes {}", row.role))
                        });
                    let normalized_payload_hash = super::super::hermes_message_revision(&row)?;
                    let provider_event_hash = format!("message:{message_id}");
                    let record_digest = decode_sha256(hermes_record_digest(values).as_str())?;
                    if &record_digest != row_version {
                        return Err(HermesSourceBackedError::StaleRecordEvidence);
                    }
                    hydrated.push(HermesHydratedMessage {
                        provider_bytes: text.as_bytes().to_vec(),
                        text,
                        provider_session_id: row.session_id,
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
        Ok(hydrated)
    }
}

#[cfg(test)]
pub(crate) fn hydrate_hermes_source_backed_message(
    data_root: &Path,
    path: &Path,
    locator: &SourceRecordLocator,
) -> HermesSourceBackedResult<HermesHydratedMessage> {
    HermesLocatorResolver::new(data_root, path, locator.source().clone()).hydrate(locator)
}

fn load_message_batch(
    conn: &rusqlite::Connection,
    schema: &HermesSchema,
    coordinates: &[(String, i64, [u8; 32])],
) -> HermesSourceBackedResult<BTreeMap<(String, i64), Vec<crate::native_source::NativeSqliteValue>>>
{
    if coordinates.is_empty() {
        return Ok(BTreeMap::new());
    }
    let visibility = schema.message_visibility();
    let visibility = if visibility.is_empty() {
        String::new()
    } else {
        format!(" and {visibility}")
    };
    let predicates = coordinates
        .iter()
        .enumerate()
        .map(|(index, _)| {
            let session = index * 2 + 1;
            let message = session + 1;
            format!("(m.session_id = ?{session} collate binary and m.id = ?{message})")
        })
        .collect::<Vec<_>>()
        .join(" or ");
    let sql = format!(
        "select {} from messages m
         where ({predicates}){visibility}
         order by m.session_id collate binary, m.id, m.rowid",
        schema.messages().projection()
    );
    let mut parameters = Vec::with_capacity(coordinates.len() * 2);
    for (provider_session_id, message_id, _) in coordinates {
        parameters.push(rusqlite::types::Value::Text(provider_session_id.clone()));
        parameters.push(rusqlite::types::Value::Integer(*message_id));
    }
    let mut statement = conn.prepare(&sql).map_err(CaptureError::from)?;
    let rows = statement
        .query_map(rusqlite::params_from_iter(parameters), |row| {
            schema.messages().capture_values(row, 0)
        })
        .map_err(CaptureError::from)?;
    let mut loaded = BTreeMap::new();
    for values in rows {
        let values = values.map_err(CaptureError::from)?;
        let row = super::super::layout::decode_hermes_message(schema, &values)?;
        let key = (row.session_id, row.id);
        let values = values
            .into_iter()
            .map(super::super::native_source_value)
            .collect();
        if loaded.insert(key, values).is_some() {
            return Err(HermesSourceBackedError::StaleRecordEvidence);
        }
    }
    Ok(loaded)
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
