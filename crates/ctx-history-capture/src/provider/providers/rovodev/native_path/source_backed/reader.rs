use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RovoDevSourceBackedDisposition {
    Cold,
    Replacement,
    Unchanged,
}

#[derive(Debug)]
pub(crate) struct RovoDevSourceBackedPage {
    pub(crate) documents: Vec<LexicalDocument>,
    pub(crate) complete_records: u64,
    pub(crate) retained_records: u64,
    pub(crate) rejected_records: u64,
    pub(crate) ignored_records: u64,
}

#[derive(Debug)]
pub(crate) struct RovoDevSourceBackedScan {
    pub(crate) disposition: RovoDevSourceBackedDisposition,
    pub(crate) source: CertifiedSource,
}

pub(crate) struct RovoDevSourceBackedReader<'a> {
    leaf: &'a RovoDevSourceBackedLeaf,
    context: ProviderAdapterContext,
    disposition: RovoDevSourceBackedDisposition,
    previous: Option<CertifiedSource>,
    next_message: usize,
    emitted_failure_document: bool,
    terminal: bool,
    counts: ScannedSourceCounts,
}

impl<'a> RovoDevSourceBackedReader<'a> {
    pub(crate) fn new(
        leaf: &'a RovoDevSourceBackedLeaf,
        context: ProviderAdapterContext,
        previous: Option<&CertifiedSource>,
    ) -> RovoDevSourceBackedResult<Self> {
        let disposition = match previous {
            None => RovoDevSourceBackedDisposition::Cold,
            Some(previous) => {
                previous.validate_contract()?;
                leaf.source_key
                    .validate_exact_descriptor(previous.observation().source())?;
                if previous.parser_revision() == PARSER_REVISION
                    && previous.observation()
                        == &leaf.snapshot.observation(leaf.source_key.clone())?
                    && previous.content_digest() == &leaf.snapshot.source_sha256
                {
                    RovoDevSourceBackedDisposition::Unchanged
                } else {
                    RovoDevSourceBackedDisposition::Replacement
                }
            }
        };
        Ok(Self {
            leaf,
            context,
            disposition,
            previous: previous.cloned(),
            next_message: 0,
            emitted_failure_document: false,
            terminal: disposition == RovoDevSourceBackedDisposition::Unchanged,
            counts: ScannedSourceCounts::default(),
        })
    }

    pub(crate) fn next_page(
        &mut self,
    ) -> RovoDevSourceBackedResult<Option<RovoDevSourceBackedPage>> {
        if self.terminal {
            return Ok(None);
        }
        let document = match self.leaf.snapshot.document.as_ref() {
            Ok(document) => document,
            Err(_) => {
                if self.emitted_failure_document {
                    self.terminal = true;
                    return Ok(None);
                }
                self.emitted_failure_document = true;
                self.terminal = true;
                let page = RovoDevSourceBackedPage {
                    documents: Vec::new(),
                    complete_records: 1,
                    retained_records: 0,
                    rejected_records: 1,
                    ignored_records: 0,
                };
                self.add_page_counts(&page)?;
                return Ok(Some(page));
            }
        };
        let start = self.next_message;
        if start > document.messages.len() {
            return Err(RovoDevSourceBackedError::CoordinateOverflow);
        }
        let mut documents = Vec::new();
        let mut rejected_records = if start == 0 {
            document.initial_failure_count
        } else {
            0
        };
        let mut ignored_records = 0_u64;
        let mut retained_bytes = 0_usize;
        let mut next = start;
        while next < document.messages.len()
            && next.saturating_sub(start) < SOURCE_BACKED_PAGE_MAX_RECORDS
        {
            let raw = &document.messages[next];
            let serialized_bytes = serde_json::to_vec(raw)
                .map_err(|error| RovoDevSourceBackedError::Capture(error.into()))?
                .len();
            if next > start
                && retained_bytes.saturating_add(serialized_bytes) > SOURCE_BACKED_PAGE_MAX_BYTES
            {
                break;
            }
            next = next
                .checked_add(1)
                .ok_or(RovoDevSourceBackedError::CoordinateOverflow)?;
            if serialized_bytes > SOURCE_BACKED_PAGE_MAX_BYTES {
                rejected_records = checked_add(rejected_records, 1)?;
                continue;
            }
            retained_bytes = retained_bytes.saturating_add(serialized_bytes);
            match project_message(raw, next.saturating_sub(1), document) {
                Err(_) => rejected_records = checked_add(rejected_records, 1)?,
                Ok(None) => ignored_records = checked_add(ignored_records, 1)?,
                Ok(Some(event)) => {
                    if event.touch_limit_exceeded {
                        rejected_records = checked_add(rejected_records, 1)?;
                    }
                    documents.push(lexical_document(
                        self.leaf,
                        document,
                        raw,
                        next.saturating_sub(1),
                        event,
                    )?);
                }
            }
        }
        let retained_records =
            u64::try_from(documents.len()).map_err(|_| RovoDevSourceBackedError::CountMismatch)?;
        let complete_records = retained_records
            .checked_add(rejected_records)
            .and_then(|count| count.checked_add(ignored_records))
            .ok_or(RovoDevSourceBackedError::CountMismatch)?;
        self.next_message = next;
        self.terminal = next == document.messages.len();
        let page = RovoDevSourceBackedPage {
            documents,
            complete_records,
            retained_records,
            rejected_records,
            ignored_records,
        };
        self.add_page_counts(&page)?;
        Ok(Some(page))
    }

    pub(crate) fn finish(mut self) -> RovoDevSourceBackedResult<RovoDevSourceBackedScan> {
        if !self.terminal {
            if self.disposition == RovoDevSourceBackedDisposition::Unchanged {
                self.terminal = true;
            } else {
                return Err(RovoDevSourceBackedError::IncompleteScan);
            }
        }
        self.leaf.snapshot.revalidate(&self.leaf.authority)?;
        let closing = RovoDevSnapshot::read(
            &self.leaf.source,
            &self.context,
            &self.leaf.authority,
            &self.leaf.session_relative_path,
            &self.leaf.context_relative_path,
            self.leaf.metadata_relative_path.as_deref(),
        )?;
        closing.revalidate(&self.leaf.authority)?;
        let opening_observation = self
            .leaf
            .snapshot
            .observation(self.leaf.source_key.clone())?;
        let closing_observation = closing.observation(self.leaf.source_key.clone())?;
        let counts = if self.disposition == RovoDevSourceBackedDisposition::Unchanged {
            self.previous
                .as_ref()
                .ok_or(RovoDevSourceBackedError::CountMismatch)?
                .counts()
        } else {
            self.counts.certified_bytes = self.leaf.snapshot.certified_bytes;
            self.counts
        };
        let frontier = final_frontier(&self.leaf.snapshot)?;
        let source = CertifiedSource::certify_with_frontier(
            opening_observation,
            closing_observation,
            PARSER_REVISION,
            self.leaf.snapshot.source_sha256,
            counts,
            Some(frontier),
        )?;
        Ok(RovoDevSourceBackedScan {
            disposition: self.disposition,
            source,
        })
    }

    fn add_page_counts(&mut self, page: &RovoDevSourceBackedPage) -> RovoDevSourceBackedResult<()> {
        self.counts.complete_records =
            checked_add(self.counts.complete_records, page.complete_records)?;
        self.counts.retained_records =
            checked_add(self.counts.retained_records, page.retained_records)?;
        self.counts.rejected_records =
            checked_add(self.counts.rejected_records, page.rejected_records)?;
        self.counts.ignored_records =
            checked_add(self.counts.ignored_records, page.ignored_records)?;
        self.counts.indexed_documents = checked_add(
            self.counts.indexed_documents,
            u64::try_from(page.documents.len())
                .map_err(|_| RovoDevSourceBackedError::CountMismatch)?,
        )?;
        Ok(())
    }
}

fn checked_add(left: u64, right: u64) -> RovoDevSourceBackedResult<u64> {
    left.checked_add(right)
        .ok_or(RovoDevSourceBackedError::CountMismatch)
}

fn final_frontier(snapshot: &RovoDevSnapshot) -> RovoDevSourceBackedResult<SourceFrontier> {
    Ok(SourceFrontier::new(
        FRONTIER_KIND,
        TypedKey::composite(vec![
            TypedKey::U64(
                u64::try_from(snapshot.message_count())
                    .map_err(|_| RovoDevSourceBackedError::CountMismatch)?,
            ),
            TypedKey::bytes(snapshot.source_sha256.to_vec())?,
        ])?,
        snapshot.certified_bytes,
        snapshot.source_sha256,
    )?)
}

fn lexical_document(
    leaf: &RovoDevSourceBackedLeaf,
    document: &PreparedDocument,
    raw_message: &serde_json::Value,
    index: usize,
    event: ProjectedMessage,
) -> RovoDevSourceBackedResult<LexicalDocument> {
    let native_item_key = native_item_key(leaf, raw_message, index)?;
    let event_id = derive_event_id(EventIdentityInput {
        source: &leaf.source_key,
        session_id: leaf.session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let message_index =
        u64::try_from(index).map_err(|_| RovoDevSourceBackedError::CoordinateOverflow)?;
    let native_record_id = provider_message_id(raw_message, message_index);
    let locator = SourceRecordLocator::new(
        leaf.source_key.clone(),
        NativeRecordCoordinate::TreeRecord {
            relative_file_key: TypedKey::utf8(RELATIVE_CONTEXT_FILE)?,
            record_coordinate: TypedKey::composite(vec![
                TypedKey::utf8(MESSAGE_OBJECT_KIND)?,
                TypedKey::U64(message_index),
                TypedKey::utf8(&native_record_id)?,
            ])?,
        },
        LocatorRevisionPolicy::ExactSourceRevision,
        Some(leaf.snapshot.source_sha256),
        leaf.snapshot.context_sha256,
    )?;
    let body = lexical_body(raw_message, event.event_type);
    Ok(LexicalDocument {
        event_id,
        session_id: leaf.session_id,
        parent_session_id: leaf.parent_session_id,
        root_session_id: leaf.root_session_id,
        source: leaf.source_key.clone(),
        locator,
        provider_session_id: Some(document.provider_session_id.clone()),
        branch: provider_string_field(
            &document.metadata,
            &[
                "branch",
                "git_branch",
                "gitBranch",
                "vcs_branch",
                "vcsBranch",
            ],
        )
        .or_else(|| document.context_branch.clone()),
        source_path: Some(leaf.source.context_path.display().to_string()),
        agent_type: if document.parent_provider_session_id.is_some() {
            AgentType::Subagent
        } else {
            AgentType::Primary
        }
        .as_str()
        .to_owned(),
        is_primary: document.parent_provider_session_id.is_none(),
        event_sequence: message_index,
        occurred_at_unix_ms: Some(event.occurred_at.timestamp_millis()),
        event_type: event.event_type.as_str().to_owned(),
        role: event.role.map(|role| role.as_str().to_owned()),
        body,
        workspace: document.cwd.clone(),
        cwd: document.cwd.clone(),
        touched_files: event.touched_files,
    })
}

fn native_item_key(
    leaf: &RovoDevSourceBackedLeaf,
    message: &serde_json::Value,
    index: usize,
) -> RovoDevSourceBackedResult<NativeItemKey> {
    if let Some(native_id) = explicit_message_id(message)
        .filter(|native_id| leaf.unique_message_ids.contains(*native_id))
    {
        return Ok(NativeItemKey::native_id(
            EVENT_KEY_NAMESPACE,
            TypedKey::utf8(native_id)?,
        )?);
    }
    let coordinate = TypedKey::composite(vec![
        explicit_message_id(message)
            .map(TypedKey::utf8)
            .transpose()?
            .unwrap_or(TypedKey::Null),
        TypedKey::U64(
            u64::try_from(index).map_err(|_| RovoDevSourceBackedError::CoordinateOverflow)?,
        ),
    ])?;
    Ok(NativeItemKey::revision_scoped_position(
        EVENT_POSITION_KIND,
        coordinate,
        TypedKey::bytes(leaf.snapshot.source_sha256.to_vec())?,
    )?)
}

fn lexical_body(
    raw_message: &serde_json::Value,
    event_type: ctx_history_core::EventType,
) -> String {
    let text = provider_block_text(raw_message).unwrap_or_default();
    if text.trim().is_empty() {
        event_type.as_str().to_owned()
    } else {
        text
    }
}

pub(crate) fn hydrate_rovodev_source_record(
    inventory: &RovoDevSourceBackedInventory,
    _event_id: StableEntityId,
    locator: &SourceRecordLocator,
) -> RovoDevSourceBackedResult<RovoDevHydratedSourceRecord> {
    locator.validate_contract()?;
    let leaf = inventory
        .leaves
        .iter()
        .find(|leaf| leaf.source_key.exact_descriptor_eq(locator.source()))
        .ok_or(RovoDevSourceBackedError::LocatorSourceMissing)?;
    if locator.source().provider() != CaptureProvider::RovoDev.as_str()
        || locator.source().source_format() != ROVODEV_SOURCE_FORMAT
        || locator.source().schema_variant() != SOURCE_SCHEMA_VARIANT
        || locator.source().provider_identity_version() != 1
        || locator.revision_policy() != LocatorRevisionPolicy::ExactSourceRevision
    {
        return Err(RovoDevSourceBackedError::LocatorSourceChanged);
    }
    let current = RovoDevSnapshot::read(
        &leaf.source,
        &inventory.context,
        &leaf.authority,
        &leaf.session_relative_path,
        &leaf.context_relative_path,
        leaf.metadata_relative_path.as_deref(),
    )?;
    if locator.certified_source_revision_digest() != Some(&current.source_sha256)
        || locator.record_digest() != &current.context_sha256
    {
        return Err(RovoDevSourceBackedError::LocatorSourceChanged);
    }
    let (message_index, expected_native_id) = decode_tree_coordinate(locator.coordinate())?;
    let document = current
        .document
        .as_ref()
        .map_err(|_| RovoDevSourceBackedError::LocatorObjectChanged)?;
    let message = document
        .messages
        .get(message_index)
        .ok_or(RovoDevSourceBackedError::LocatorObjectChanged)?;
    let observed_native_id = provider_message_id(
        message,
        u64::try_from(message_index).map_err(|_| RovoDevSourceBackedError::CoordinateOverflow)?,
    );
    if observed_native_id != expected_native_id {
        return Err(RovoDevSourceBackedError::LocatorObjectChanged);
    }
    let role_text = message
        .get("role")
        .or_else(|| message.get("kind"))
        .or_else(|| message.get("type"))
        .and_then(serde_json::Value::as_str);
    let decoded_display_text = lexical_body(message, rovodev_event_type(message, role_text));
    let provider_bytes = decoded_display_text.as_bytes().to_vec();
    current.revalidate(&leaf.authority)?;
    let closing = RovoDevSnapshot::read(
        &leaf.source,
        &inventory.context,
        &leaf.authority,
        &leaf.session_relative_path,
        &leaf.context_relative_path,
        leaf.metadata_relative_path.as_deref(),
    )?;
    closing.revalidate(&leaf.authority)?;
    if closing.source_sha256 != current.source_sha256
        || closing.context_sha256 != current.context_sha256
    {
        return Err(RovoDevSourceBackedError::LocatorSourceChanged);
    }
    Ok(RovoDevHydratedSourceRecord {
        provider_bytes,
        decoded_display_text: Some(decoded_display_text),
    })
}

fn decode_tree_coordinate(
    coordinate: &NativeRecordCoordinate,
) -> RovoDevSourceBackedResult<(usize, String)> {
    let NativeRecordCoordinate::TreeRecord {
        relative_file_key,
        record_coordinate,
    } = coordinate
    else {
        return Err(RovoDevSourceBackedError::InvalidLocator);
    };
    let TypedKey::Utf8(relative_file) = relative_file_key else {
        return Err(RovoDevSourceBackedError::InvalidLocator);
    };
    let TypedKey::Composite(parts) = record_coordinate else {
        return Err(RovoDevSourceBackedError::InvalidLocator);
    };
    let [TypedKey::Utf8(object_kind), TypedKey::U64(message_index), TypedKey::Utf8(native_id)] =
        parts.as_slice()
    else {
        return Err(RovoDevSourceBackedError::InvalidLocator);
    };
    if relative_file != RELATIVE_CONTEXT_FILE
        || object_kind != MESSAGE_OBJECT_KIND
        || native_id.is_empty()
    {
        return Err(RovoDevSourceBackedError::InvalidLocator);
    }
    Ok((
        usize::try_from(*message_index)
            .map_err(|_| RovoDevSourceBackedError::CoordinateOverflow)?,
        native_id.clone(),
    ))
}
