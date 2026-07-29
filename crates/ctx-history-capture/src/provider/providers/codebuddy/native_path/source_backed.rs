//! Provider-local source-backed projection for CodeBuddy's two native stores.
//!
//! The IDE extension stores whole JSON message documents while the CLI stores
//! append-oriented JSONL. They share the existing bounded parser and
//! normalization, but retain independent source identities, certification,
//! locator contracts, and replacement evidence.

use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, EventIdentityInput, LocatorRevisionPolicy,
    NativeItemKey, NativeRecordCoordinate, NativeSessionKey, ScannedSourceCounts,
    SessionIdentityInput, SourceAnchor, SourceFrontier, SourceKey, SourceObservation,
    SourceRecordLocator, TypedKey,
};
use ctx_history_index::LexicalDocument;

use crate::provider::provider_safe_path_segment;

use super::*;

const CODEBUDDY_SOURCE_BACKED_IDENTITY_VERSION: u32 = 1;
const CODEBUDDY_SOURCE_BACKED_PARSER_REVISION: &str = "codebuddy-source-backed-v1";
const CODEBUDDY_SOURCE_ANCHOR_NAMESPACE: &str = "codebuddy-native-source-v1";
const CODEBUDDY_SESSION_KEY_NAMESPACE: &str = "codebuddy-native-session-v1";
const CODEBUDDY_EVENT_KEY_NAMESPACE: &str = "codebuddy-native-event-v1";
const CODEBUDDY_CLI_SCHEMA_VARIANT: &str = "cli-jsonl-v1";
const CODEBUDDY_EXTENSION_SCHEMA_VARIANT: &str = "ide-structured-message-v1";
const CODEBUDDY_CLI_LOCATOR_TAG: &str = "codebuddy-jsonl-range-v1";
const CODEBUDDY_EXTENSION_LOCATOR_TAG: &str = "codebuddy-structured-message-v1";
const CODEBUDDY_CLI_FRONTIER_KIND: &str = "codebuddy-jsonl-frontier-v1";
const CODEBUDDY_EXTENSION_CANONICAL_DOMAIN: &[u8] = b"ctx-codebuddy-structured-source-v1\0";

/// One parser-bounded page ready for the shared lexical generation writer.
#[derive(Debug, Clone)]
pub(crate) struct CodeBuddySourceBackedPage {
    pub(crate) documents: Vec<LexicalDocument>,
    pub(crate) complete_records: u64,
    pub(crate) retained_records: u64,
    pub(crate) rejected_records: u64,
    pub(crate) ignored_records: u64,
}

/// A bounded record-local rejection retained as scan evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodeBuddySourceBackedRejection {
    pub(crate) line: usize,
    pub(crate) detail: String,
}

/// A complete certified provider source and its parser-bounded lexical pages.
#[derive(Debug, Clone)]
pub(crate) struct CodeBuddySourceBackedScan {
    pub(crate) source: CertifiedSource,
    pub(crate) pages: Vec<CodeBuddySourceBackedPage>,
    pub(crate) rejections: Vec<CodeBuddySourceBackedRejection>,
}

/// Exact provider bytes and the display text decoded through CodeBuddy's
/// existing format-specific complete-content parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodeBuddyHydratedSourceRecord {
    pub(crate) provider_bytes: Vec<u8>,
    pub(crate) decoded_display_text: String,
}

pub(crate) fn codebuddy_cli_complete_content_record(
    value: &Value,
    physical_line: usize,
) -> Option<(String, String)> {
    let text = cli_message_text(value);
    if !codebuddy_is_message_record(
        value.get("role").and_then(Value::as_str),
        value.get("type").and_then(Value::as_str),
    ) || text.trim().is_empty()
    {
        return None;
    }
    let native_record_id = codebuddy_cli_explicit_native_message_id(value)
        .unwrap_or_else(|| format!("line-{physical_line}"));
    Some((text, native_record_id))
}

pub(crate) fn codebuddy_cli_complete_content_source_from_admitted(
    metadata: &Metadata,
    path_identity: String,
) -> Result<(String, String)> {
    let frozen = CodeBuddyFrozenFile::from_metadata(metadata)?;
    Ok((
        frozen.source_revision_with_policy("cli-jsonl", CODEBUDDY_CLI_POLICY_REVISION),
        path_identity,
    ))
}

/// Discovers and scans every CodeBuddy source below one configured store root.
///
/// CLI JSONL files and IDE extension session directories remain separate
/// certified sources even when their provider-native project/session IDs are
/// equal.
pub(crate) fn scan_codebuddy_source_backed_root(
    root: &Path,
    imported_at: DateTime<Utc>,
) -> Result<Vec<CodeBuddySourceBackedScan>> {
    let context = ProviderAdapterContext {
        machine_id: "source-backed-codebuddy".to_owned(),
        source_path: Some(root.to_path_buf()),
        source_root: Some(root.to_path_buf()),
        imported_at,
    };
    let mut inventory = discover_sources(root, &ProviderImportOptions::default())?;
    if inventory.root_missing {
        return Ok(Vec::new());
    }
    let authority = codebuddy_authority(root)?;
    for source in &mut inventory.sources {
        bind_codebuddy_capability(source, &authority)?;
    }
    inventory
        .sources
        .iter()
        .map(|source| scan_source(source, &context))
        .collect()
}

/// Resolves one typed source-backed locator against the currently installed
/// CodeBuddy stores and fails closed on stale source or record evidence.
pub(crate) fn hydrate_codebuddy_source_backed_record(
    root: &Path,
    locator: &SourceRecordLocator,
) -> Result<CodeBuddyHydratedSourceRecord> {
    contract(locator.validate_contract(), "locator")?;
    let mut inventory = discover_sources(root, &ProviderImportOptions::default())?;
    let authority = codebuddy_authority(root)?;
    for source in &mut inventory.sources {
        bind_codebuddy_capability(source, &authority)?;
    }
    let mut matched = None;
    for source in &inventory.sources {
        let state = initial_state(
            source,
            &ProviderAdapterContext {
                machine_id: "source-backed-codebuddy-hydration".to_owned(),
                source_path: Some(root.to_path_buf()),
                source_root: Some(root.to_path_buf()),
                imported_at: DateTime::<Utc>::from_timestamp(0, 0).ok_or(
                    CaptureError::SystemInvariant("Unix epoch must be representable"),
                )?,
            },
        )?;
        let source_key = codebuddy_source_key(source, &state.session)?;
        if source_key.exact_descriptor_eq(locator.source()) {
            if matched.replace((source, state)).is_some() {
                return Err(invalid_source_backed(
                    "locator source identity matched multiple installed CodeBuddy stores",
                ));
            }
        }
    }
    let (source, state) = matched.ok_or_else(|| {
        invalid_source_backed("locator source is not present in the configured CodeBuddy stores")
    })?;
    let hydrated = match source.shape {
        CodeBuddySourceShape::Cli => hydrate_cli(source, &state.session, locator),
        CodeBuddySourceShape::Extension => hydrate_extension(source, &state.session, locator),
    }?;
    revalidate_codebuddy_capability(source)?;
    Ok(hydrated)
}

fn scan_source(
    source: &CodeBuddySource,
    context: &ProviderAdapterContext,
) -> Result<CodeBuddySourceBackedScan> {
    let mut state = initial_state(source, context)?;
    let source_key = codebuddy_source_key(source, &state.session)?;
    let opening = source_observation(source, source_key.clone())?;
    let certified_revision_digest = source_revision_digest(source);
    let mut pages = Vec::new();
    let mut counts = ScannedSourceCounts::default();
    let mut structured_digest = Sha256::new();
    let mut structured_bytes = 0_u64;
    if source.shape == CodeBuddySourceShape::Extension {
        structured_digest.update(CODEBUDDY_EXTENSION_CANONICAL_DOMAIN);
        structured_digest.update(source.source_revision.as_bytes());
        structured_bytes = u64::try_from(source.source_revision.len())
            .map_err(|_| CaptureError::SystemInvariant("CodeBuddy source revision exceeds u64"))?;
    }

    while let Some(page) = next_source_page(source, &state, context)? {
        let mut projected = CodeBuddySourceBackedPage {
            documents: Vec::new(),
            complete_records: 0,
            retained_records: 0,
            rejected_records: 0,
            ignored_records: 0,
        };
        for record in &page.records {
            projected.complete_records =
                projected
                    .complete_records
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "CodeBuddy complete record count overflowed",
                    ))?;
            match &record.classification {
                CodeBuddyRecordClassification::AcceptedMessage(core) => {
                    projected.retained_records = projected.retained_records.checked_add(1).ok_or(
                        CaptureError::SystemInvariant("CodeBuddy retained record count overflowed"),
                    )?;
                    projected.documents.push(codebuddy_lexical_document(
                        source,
                        &source_key,
                        certified_revision_digest,
                        record,
                        core,
                    )?);
                }
                CodeBuddyRecordClassification::RejectedRecord => {
                    projected.rejected_records = projected.rejected_records.checked_add(1).ok_or(
                        CaptureError::SystemInvariant("CodeBuddy rejected record count overflowed"),
                    )?;
                }
                CodeBuddyRecordClassification::SkippedMetadata => {
                    projected.ignored_records = projected.ignored_records.checked_add(1).ok_or(
                        CaptureError::SystemInvariant("CodeBuddy ignored record count overflowed"),
                    )?;
                }
            }
            if source.shape == CodeBuddySourceShape::Extension {
                structured_digest.update(record.native_ordinal.to_be_bytes());
                structured_digest.update((record.native_bytes.len() as u64).to_be_bytes());
                structured_digest.update(&record.native_bytes);
                structured_bytes = structured_bytes
                    .checked_add(16)
                    .and_then(|value| value.checked_add(record.native_bytes.len() as u64))
                    .ok_or(CaptureError::SystemInvariant(
                        "CodeBuddy structured source byte count overflowed",
                    ))?;
            }
        }
        merge_page_counts(&mut counts, &projected)?;
        pages.push(projected);
        state = page.next_state;
    }

    if !state.terminal {
        return Err(CaptureError::SystemInvariant(
            "CodeBuddy source-backed scan stopped before the terminal frontier",
        ));
    }
    revalidate_codebuddy_capability(source)?;

    let (content_digest, certified_bytes, frontier) = match source.shape {
        CodeBuddySourceShape::Cli => {
            let digest = decode_sha256(&state.certified_prefix_sha256)?;
            let frontier = contract(
                SourceFrontier::new(
                    CODEBUDDY_CLI_FRONTIER_KIND,
                    contract(
                        TypedKey::composite(vec![
                            TypedKey::U64(state.next_native_offset),
                            TypedKey::U64(state.next_native_ordinal),
                        ]),
                        "CLI frontier key",
                    )?,
                    state.next_native_offset,
                    digest,
                ),
                "CLI frontier",
            )?;
            (digest, state.next_native_offset, Some(frontier))
        }
        CodeBuddySourceShape::Extension => {
            (structured_digest.finalize().into(), structured_bytes, None)
        }
    };
    counts.certified_bytes = certified_bytes;
    let closing = source_observation(source, source_key)?;
    let certified = contract(
        CertifiedSource::certify_with_frontier(
            opening,
            closing,
            CODEBUDDY_SOURCE_BACKED_PARSER_REVISION,
            content_digest,
            counts,
            frontier,
        ),
        "source certification",
    )?;
    let rejections = state
        .failures
        .iter()
        .chain(state.incomplete_tail.iter())
        .map(|failure| CodeBuddySourceBackedRejection {
            line: failure.line,
            detail: failure.error.clone(),
        })
        .collect();
    Ok(CodeBuddySourceBackedScan {
        source: certified,
        pages,
        rejections,
    })
}

fn codebuddy_authority(root: &Path) -> Result<ProviderSourceRoot> {
    let selected = fs::canonicalize(root)?;
    let authority_path = if fs::metadata(root)?.is_file() {
        selected
            .parent()
            .ok_or(CaptureError::InvalidProviderTranscriptPath {
                path: selected.clone(),
                reason: "CodeBuddy selected file has no authority directory",
            })?
            .to_path_buf()
    } else {
        selected
    };
    ProviderSourceRoot::open(&authority_path)
}

fn bind_codebuddy_capability(
    source: &mut CodeBuddySource,
    authority: &ProviderSourceRoot,
) -> Result<()> {
    let relative_path = source
        .canonical_path
        .strip_prefix(authority.named_path())
        .map(Path::to_path_buf)
        .map_err(|_| CaptureError::InvalidProviderTranscriptPath {
            path: source.canonical_path.clone(),
            reason: "CodeBuddy compound leaves must share one authority root",
        })?;
    let capability = match source.shape {
        CodeBuddySourceShape::Cli => {
            let primary = authority.open_file(&relative_path)?;
            let frozen = CodeBuddyFrozenFile::from_metadata(primary.metadata())?;
            let base_revision =
                frozen.source_revision_with_policy("cli-jsonl", CODEBUDDY_CLI_POLICY_REVISION);
            let revision = effective_source_revision(
                &base_revision,
                source.inventory_observation_token.as_deref(),
            );
            source.frozen = Some(frozen);
            source.base_source_revision = base_revision;
            source.source_revision.clone_from(&revision);
            CodeBuddyCapabilitySource {
                authority: authority.clone(),
                primary: Some(primary),
                extension: None,
                revision,
            }
        }
        CodeBuddySourceShape::Extension => {
            let session_index_relative = relative_path.join("index.json");
            let messages_relative = relative_path.join("messages");
            let project_index_relative = relative_path
                .parent()
                .unwrap_or_else(|| Path::new(""))
                .join("index.json");
            let session_index = authority.open_file(&session_index_relative)?;
            let project_index = match authority.open_file(&project_index_relative) {
                Ok(file) => Some(file),
                Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => return Err(error),
            };
            let messages_directory = authority.open_directory(&messages_relative)?;
            let session_index_bytes =
                session_index.read_all_bounded(MAX_PROVIDER_JSONL_LINE_BYTES)?;
            let project_index_bytes = project_index
                .as_ref()
                .map(|file| file.read_all_bounded(MAX_PROVIDER_JSONL_LINE_BYTES))
                .transpose()?;
            let metadata = codebuddy_extension_metadata_from_admitted(
                &source.path,
                &session_index_bytes,
                project_index_bytes.as_deref(),
            )?;
            let mut messages = BTreeMap::new();
            let mut revision = CodeBuddyRevisionHasher::new();
            revision.update(b"codebuddy-extension-capability-v1");
            revision.update(&session_index_bytes);
            match project_index_bytes.as_deref() {
                Some(bytes) => {
                    revision.update(b"project-index");
                    revision.update(bytes);
                }
                None => revision.update(b"missing-project-index"),
            }
            for message_ref in metadata.messages() {
                serde_json::to_writer(&mut revision, message_ref)?;
                let Some(message_id) = message_ref
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| provider_safe_path_segment(id))
                else {
                    revision.update(b"rejected-message-id");
                    continue;
                };
                let relative = messages_relative.join(format!("{message_id}.json"));
                let file = authority.open_file(&relative)?;
                let frozen = CodeBuddyFrozenFile::from_metadata(file.metadata())?;
                frozen.update_revision(&mut revision);
                messages.insert(message_id.to_owned(), file);
            }
            let base_revision = format!(
                "codebuddy-extension-capability-v1:fnv1a64:{:016x}",
                revision.finish()
            );
            let effective_revision = effective_source_revision(
                &base_revision,
                source.inventory_observation_token.as_deref(),
            );
            source.base_source_revision = base_revision;
            source.source_revision.clone_from(&effective_revision);
            CodeBuddyCapabilitySource {
                authority: authority.clone(),
                primary: None,
                extension: Some(CodeBuddyExtensionCapability {
                    metadata,
                    session_index,
                    project_index,
                    messages_directory,
                    messages,
                }),
                revision: effective_revision,
            }
        }
    };
    source.capability = Some(Arc::new(capability));
    Ok(())
}

fn revalidate_codebuddy_capability(source: &CodeBuddySource) -> Result<()> {
    let capability = source
        .capability
        .as_ref()
        .ok_or(CaptureError::SystemInvariant(
            "CodeBuddy source-backed source lost its authority capability",
        ))?;
    capability.revalidate()?;
    let mut closing = source.clone();
    closing.capability = None;
    bind_codebuddy_capability(&mut closing, &capability.authority)?;
    let closing_capability = closing
        .capability
        .as_ref()
        .ok_or(CaptureError::SystemInvariant(
            "CodeBuddy closing admission lost its authority capability",
        ))?;
    closing_capability.revalidate()?;
    if closing_capability.revision != capability.revision {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    Ok(())
}

fn codebuddy_source_key(
    source: &CodeBuddySource,
    session: &CodeBuddySessionState,
) -> Result<SourceKey> {
    let (shape, schema_variant) = match source.shape {
        CodeBuddySourceShape::Cli => ("cli", CODEBUDDY_CLI_SCHEMA_VARIANT),
        CodeBuddySourceShape::Extension => ("ide", CODEBUDDY_EXTENSION_SCHEMA_VARIANT),
    };
    let anchor_key = contract(
        TypedKey::composite(vec![
            contract(TypedKey::utf8(shape), "source shape key")?,
            contract(
                TypedKey::utf8(session.project_hash.clone()),
                "project source key",
            )?,
            contract(
                TypedKey::utf8(session.native_session_id.clone()),
                "session source key",
            )?,
        ]),
        "source anchor key",
    )?;
    let anchor = contract(
        SourceAnchor::provider_native(CODEBUDDY_SOURCE_ANCHOR_NAMESPACE, anchor_key),
        "source anchor",
    )?;
    contract(
        SourceKey::derive(
            CaptureProvider::CodeBuddy.as_str(),
            CODEBUDDY_SOURCE_FORMAT,
            schema_variant,
            CODEBUDDY_SOURCE_BACKED_IDENTITY_VERSION,
            anchor,
        ),
        "source key",
    )
}

fn source_observation(source: &CodeBuddySource, key: SourceKey) -> Result<SourceObservation> {
    contract(
        SourceObservation::new(
            key,
            format!("codebuddy-{}-observation-v1", source.shape.shape_tag()),
            source.source_revision.as_bytes().to_vec(),
        ),
        "source observation",
    )
}

fn source_revision_digest(source: &CodeBuddySource) -> [u8; 32] {
    Sha256::digest(source.source_revision.as_bytes()).into()
}

fn merge_page_counts(
    counts: &mut ScannedSourceCounts,
    page: &CodeBuddySourceBackedPage,
) -> Result<()> {
    counts.complete_records = counts
        .complete_records
        .checked_add(page.complete_records)
        .ok_or(CaptureError::SystemInvariant(
            "CodeBuddy complete record count overflowed",
        ))?;
    counts.retained_records = counts
        .retained_records
        .checked_add(page.retained_records)
        .ok_or(CaptureError::SystemInvariant(
            "CodeBuddy retained record count overflowed",
        ))?;
    counts.rejected_records = counts
        .rejected_records
        .checked_add(page.rejected_records)
        .ok_or(CaptureError::SystemInvariant(
            "CodeBuddy rejected record count overflowed",
        ))?;
    counts.ignored_records = counts
        .ignored_records
        .checked_add(page.ignored_records)
        .ok_or(CaptureError::SystemInvariant(
            "CodeBuddy ignored record count overflowed",
        ))?;
    counts.indexed_documents = counts
        .indexed_documents
        .checked_add(page.documents.len() as u64)
        .ok_or(CaptureError::SystemInvariant(
            "CodeBuddy indexed document count overflowed",
        ))?;
    Ok(())
}

fn codebuddy_lexical_document(
    source: &CodeBuddySource,
    source_key: &SourceKey,
    certified_revision_digest: [u8; 32],
    record: &CodeBuddyRecord,
    core: &CodeBuddyCoreRow,
) -> Result<LexicalDocument> {
    let provider_session_id = core.session.provider_session_id.clone();
    let session_key = contract(
        NativeSessionKey::native_id(
            CODEBUDDY_SESSION_KEY_NAMESPACE,
            contract(
                TypedKey::utf8(provider_session_id.clone()),
                "native session key",
            )?,
        ),
        "native session key",
    )?;
    let session_id = contract(
        derive_session_id(SessionIdentityInput {
            source: source_key,
            logical_session_kind: "codebuddy-session",
            native_session_key: &session_key,
        }),
        "session identity",
    )?;
    let native_message_id = core.event.native_message_id.as_str();
    if native_message_id.is_empty() {
        return Err(CaptureError::SystemInvariant(
            "CodeBuddy normalized event lost its native message identity",
        ));
    }
    let item_key = contract(
        NativeItemKey::native_id(
            CODEBUDDY_EVENT_KEY_NAMESPACE,
            contract(
                TypedKey::composite(vec![
                    contract(TypedKey::utf8(source.shape.shape_tag()), "event shape key")?,
                    contract(TypedKey::utf8(native_message_id), "native message key")?,
                ]),
                "native event key",
            )?,
        ),
        "native event key",
    )?;
    let event_id = contract(
        derive_event_id(EventIdentityInput {
            source: source_key,
            session_id,
            logical_item_kind: "codebuddy-event",
            native_item_key: &item_key,
            subrecord_selector: None,
        }),
        "event identity",
    )?;
    let record_digest: [u8; 32] = Sha256::digest(&record.native_bytes).into();
    let locator = match source.shape {
        CodeBuddySourceShape::Cli => cli_locator(
            source_key,
            record,
            &provider_session_id,
            native_message_id,
            record_digest,
        )?,
        CodeBuddySourceShape::Extension => extension_locator(
            source_key,
            record,
            native_message_id,
            &core.event.legacy_provider_event_hash,
            certified_revision_digest,
            record_digest,
        )?,
    };
    Ok(LexicalDocument {
        event_id,
        session_id,
        parent_session_id: None,
        root_session_id: session_id,
        source: source_key.clone(),
        locator,
        provider_session_id: Some(provider_session_id),
        branch: None,
        source_path: Some(source.canonical_path.display().to_string()),
        agent_type: AgentType::Primary.as_str().to_owned(),
        is_primary: true,
        event_sequence: record.native_ordinal,
        occurred_at_unix_ms: Some(core.event.occurred_at.timestamp_millis()),
        event_type: core.event.event_type.as_str().to_owned(),
        role: Some(core.event.role.as_str().to_owned()),
        body: lexical_body(&core.event)?,
        workspace: None,
        cwd: core.session.cwd.clone(),
        touched_files: Vec::new(),
    })
}

fn cli_locator(
    source: &SourceKey,
    record: &CodeBuddyRecord,
    provider_session_id: &str,
    native_message_id: &str,
    record_digest: [u8; 32],
) -> Result<SourceRecordLocator> {
    let byte_offset = record.byte_start.ok_or(CaptureError::SystemInvariant(
        "CodeBuddy CLI record lost its byte offset",
    ))?;
    let byte_end = record
        .byte_end_exclusive
        .ok_or(CaptureError::SystemInvariant(
            "CodeBuddy CLI record lost its byte end",
        ))?;
    let byte_length = byte_end
        .checked_sub(byte_offset)
        .filter(|length| *length != 0)
        .ok_or(CaptureError::SystemInvariant(
            "CodeBuddy CLI record has an invalid byte range",
        ))?;
    let event_key = contract(
        TypedKey::composite(vec![
            contract(TypedKey::utf8(CODEBUDDY_CLI_LOCATOR_TAG), "CLI tag")?,
            contract(TypedKey::utf8(native_message_id), "CLI native message key")?,
        ]),
        "CLI locator event key",
    )?;
    contract(
        SourceRecordLocator::new(
            source.clone(),
            NativeRecordCoordinate::Jsonl {
                byte_offset,
                byte_length,
                physical_ordinal: record.native_ordinal,
                native_session_key: Some(contract(
                    TypedKey::utf8(provider_session_id),
                    "CLI locator session key",
                )?),
                native_event_key: Some(event_key),
            },
            LocatorRevisionPolicy::StableRecordEvidence,
            None,
            record_digest,
        ),
        "CLI source record locator",
    )
}

fn extension_locator(
    source: &SourceKey,
    record: &CodeBuddyRecord,
    message_id: &str,
    native_record_id: &str,
    certified_revision_digest: [u8; 32],
    record_digest: [u8; 32],
) -> Result<SourceRecordLocator> {
    if !provider_safe_path_segment(message_id) {
        return Err(invalid_source_backed(
            "structured message identity is not a safe path segment",
        ));
    }
    let coordinate = contract(
        TypedKey::composite(vec![
            contract(
                TypedKey::utf8(CODEBUDDY_EXTENSION_LOCATOR_TAG),
                "structured locator tag",
            )?,
            TypedKey::U64(record.native_ordinal),
            contract(
                TypedKey::utf8(native_record_id),
                "structured native record key",
            )?,
        ]),
        "structured record coordinate",
    )?;
    contract(
        SourceRecordLocator::new(
            source.clone(),
            NativeRecordCoordinate::TreeRecord {
                relative_file_key: contract(
                    TypedKey::utf8(format!("messages/{message_id}.json")),
                    "structured relative file key",
                )?,
                record_coordinate: coordinate,
            },
            LocatorRevisionPolicy::ExactSourceRevision,
            Some(certified_revision_digest),
            record_digest,
        ),
        "structured source record locator",
    )
}

fn lexical_body(event: &CodeBuddyEventDraft) -> Result<String> {
    Ok(if event.text.trim().is_empty() {
        event.event_type.as_str().to_owned()
    } else {
        event.text.clone()
    })
}

fn hydrate_cli(
    source: &CodeBuddySource,
    session: &CodeBuddySessionState,
    locator: &SourceRecordLocator,
) -> Result<CodeBuddyHydratedSourceRecord> {
    if locator.revision_policy() != LocatorRevisionPolicy::StableRecordEvidence
        || locator.certified_source_revision_digest().is_some()
    {
        return Err(invalid_source_backed(
            "CLI locator has the wrong revision policy",
        ));
    }
    let NativeRecordCoordinate::Jsonl {
        byte_offset,
        byte_length,
        physical_ordinal,
        native_session_key,
        native_event_key,
    } = locator.coordinate()
    else {
        return Err(invalid_source_backed(
            "CLI locator is not a JSONL byte range",
        ));
    };
    if *byte_length > CODEBUDDY_NATIVE_RECORD_MAX_BYTES as u64 {
        return Err(invalid_source_backed(
            "CLI locator exceeds the bounded record size",
        ));
    }
    let provider_bytes = match source
        .capability
        .as_ref()
        .and_then(|capability| capability.primary.as_ref())
    {
        Some(file) => file.read_exact_range(
            *byte_offset,
            usize::try_from(*byte_length)
                .map_err(|_| invalid_source_backed("CLI locator range is too large"))?,
            CODEBUDDY_NATIVE_RECORD_MAX_BYTES,
        )?,
        None => read_exact_range(&source.path, *byte_offset, *byte_length)?,
    };
    let payload = jsonl_payload(&provider_bytes);
    if Sha256::digest(payload).as_slice() != locator.record_digest() {
        return Err(invalid_source_backed(
            "CLI locator record digest no longer matches provider bytes",
        ));
    }
    let value: Value = serde_json::from_slice(payload)?;
    let physical_line = usize::try_from(*physical_ordinal)
        .ok()
        .and_then(|ordinal| ordinal.checked_add(1))
        .ok_or(CaptureError::SystemInvariant(
            "CodeBuddy physical line exceeds platform limits",
        ))?;
    let (text, native_message_id) = codebuddy_cli_complete_content_record(&value, physical_line)
        .ok_or_else(|| {
            invalid_source_backed("CLI locator no longer resolves to a CodeBuddy message")
        })?;
    let expected_session = session_key_utf8(native_session_key.as_ref())
        .ok_or_else(|| invalid_source_backed("CLI locator has an invalid native session key"))?;
    let observed_session = value
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(|value| format!("{}/{}", session.project_hash, value))
        .unwrap_or_else(|| session.provider_session_id());
    if expected_session != observed_session
        || !tagged_event_key_matches(
            native_event_key.as_ref(),
            CODEBUDDY_CLI_LOCATOR_TAG,
            &native_message_id,
        )
    {
        return Err(invalid_source_backed(
            "CLI locator native identity no longer matches the provider record",
        ));
    }
    Ok(CodeBuddyHydratedSourceRecord {
        provider_bytes: text.as_bytes().to_vec(),
        decoded_display_text: text,
    })
}

fn hydrate_extension(
    source: &CodeBuddySource,
    session: &CodeBuddySessionState,
    locator: &SourceRecordLocator,
) -> Result<CodeBuddyHydratedSourceRecord> {
    if locator.revision_policy() != LocatorRevisionPolicy::ExactSourceRevision
        || locator.certified_source_revision_digest() != Some(&source_revision_digest(source))
    {
        return Err(invalid_source_backed(
            "structured locator source revision is stale",
        ));
    }
    let (relative_path, ordinal, native_record_id) = structured_coordinate(locator.coordinate())?;
    let message_id = relative_path
        .strip_prefix("messages/")
        .and_then(|value| value.strip_suffix(".json"))
        .filter(|value| provider_safe_path_segment(value))
        .ok_or_else(|| invalid_source_backed("structured locator message path is invalid"))?;
    let expected_native_record_id = format!("{}:{message_id}", session.provider_session_id());
    if native_record_id != expected_native_record_id {
        return Err(invalid_source_backed(
            "structured locator native identity does not match its source",
        ));
    }
    let path = source.path.join(&relative_path);
    let admitted = source
        .capability
        .as_ref()
        .and_then(|capability| capability.extension.as_ref())
        .and_then(|extension| extension.messages.get(message_id));
    let frozen = match admitted {
        Some(file) => CodeBuddyFrozenFile::from_metadata(file.metadata())?,
        None => CodeBuddyFrozenFile::read(&path)?,
    };
    if frozen.length > CODEBUDDY_NATIVE_RECORD_MAX_BYTES as u64 {
        return Err(invalid_source_backed(
            "structured locator exceeds the bounded record size",
        ));
    }
    let provider_bytes = match admitted {
        Some(file) => file.read_all_bounded(CODEBUDDY_NATIVE_RECORD_MAX_BYTES)?,
        None => fs::read(&path)?,
    };
    let revalidated = match admitted {
        Some(file) => file.revalidate().is_ok(),
        None => frozen.revalidate(&path)?,
    };
    if !revalidated || Sha256::digest(&provider_bytes).as_slice() != locator.record_digest() {
        return Err(invalid_source_backed(
            "structured locator record digest no longer matches provider bytes",
        ));
    }
    let raw: Value = serde_json::from_slice(&provider_bytes)?;
    let decoded = codebuddy_decoded_message(&raw);
    let text = codebuddy_message_text(&decoded, &raw);
    if text.trim().is_empty() {
        return Err(invalid_source_backed(
            "structured locator no longer resolves to displayable message content",
        ));
    }
    let _ = ordinal;
    Ok(CodeBuddyHydratedSourceRecord {
        provider_bytes: text.as_bytes().to_vec(),
        decoded_display_text: text,
    })
}

fn structured_coordinate(coordinate: &NativeRecordCoordinate) -> Result<(String, u64, String)> {
    let NativeRecordCoordinate::TreeRecord {
        relative_file_key,
        record_coordinate,
    } = coordinate
    else {
        return Err(invalid_source_backed(
            "structured locator is not a tree record",
        ));
    };
    let TypedKey::Utf8(relative_path) = relative_file_key else {
        return Err(invalid_source_backed(
            "structured locator relative path is not UTF-8",
        ));
    };
    let TypedKey::Composite(parts) = record_coordinate else {
        return Err(invalid_source_backed(
            "structured locator coordinate is not tagged",
        ));
    };
    match parts.as_slice() {
        [TypedKey::Utf8(tag), TypedKey::U64(ordinal), TypedKey::Utf8(native_id)]
            if tag == CODEBUDDY_EXTENSION_LOCATOR_TAG =>
        {
            Ok((relative_path.clone(), *ordinal, native_id.clone()))
        }
        _ => Err(invalid_source_backed(
            "structured locator coordinate has the wrong format tag",
        )),
    }
}

fn tagged_event_key_matches(key: Option<&TypedKey>, tag: &str, native_id: &str) -> bool {
    matches!(
        key,
        Some(TypedKey::Composite(parts))
            if matches!(
                parts.as_slice(),
                [TypedKey::Utf8(actual_tag), TypedKey::Utf8(actual_id)]
                    if actual_tag == tag && actual_id == native_id
            )
    )
}

fn session_key_utf8(key: Option<&TypedKey>) -> Option<&str> {
    match key {
        Some(TypedKey::Utf8(value)) => Some(value),
        _ => None,
    }
}

fn read_exact_range(path: &Path, offset: u64, length: u64) -> Result<Vec<u8>> {
    let end = offset
        .checked_add(length)
        .ok_or_else(|| invalid_source_backed("source record range overflowed"))?;
    let mut file = File::open(path)?;
    if end > file.metadata()?.len() {
        return Err(invalid_source_backed(
            "source record range ends after the provider source",
        ));
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = vec![
        0_u8;
        usize::try_from(length).map_err(|_| {
            invalid_source_backed("source record range exceeds platform limits")
        })?
    ];
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn jsonl_payload(bytes: &[u8]) -> &[u8] {
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    bytes.strip_suffix(b"\r").unwrap_or(bytes)
}

fn decode_sha256(value: &str) -> Result<[u8; 32]> {
    if value.len() != 64 {
        return Err(invalid_source_backed(
            "parser frontier has an invalid SHA-256 digest",
        ));
    }
    let mut digest = [0_u8; 32];
    for (index, slot) in digest.iter_mut().enumerate() {
        let start = index.saturating_mul(2);
        *slot = u8::from_str_radix(&value[start..start + 2], 16)
            .map_err(|_| invalid_source_backed("parser frontier has a non-hex SHA-256 digest"))?;
    }
    Ok(digest)
}

fn contract<T, E: std::fmt::Display>(
    result: std::result::Result<T, E>,
    boundary: &'static str,
) -> Result<T> {
    result.map_err(|error| {
        invalid_source_backed(format!("{boundary} violates the shared contract: {error}"))
    })
}

fn invalid_source_backed(detail: impl Into<String>) -> CaptureError {
    CaptureError::InvalidPayload(format!(
        "CodeBuddy source-backed adapter: {}",
        detail.into()
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use crate::test_support_paths::tempdir;

    use super::*;

    const IMPORTED_AT: &str = "2026-07-28T12:00:00Z";

    fn write_dual_store(root: &Path, cli_text: &str, extension_text: &str) {
        let cli = root.join("projects/shared-project/shared-session.jsonl");
        fs::create_dir_all(cli.parent().unwrap()).unwrap();
        fs::write(
            &cli,
            format!(
                "{}\n",
                serde_json::to_string(&json!({
                    "id": "cli-message",
                    "type": "message",
                    "role": "user",
                    "content": cli_text,
                    "timestamp": IMPORTED_AT,
                    "sessionId": "shared-session",
                    "cwd": "/workspace/codebuddy-cli",
                }))
                .unwrap()
            ),
        )
        .unwrap();

        let project = root.join("history/shared-project");
        let session = project.join("shared-session");
        fs::create_dir_all(session.join("messages")).unwrap();
        fs::write(
            session.join("index.json"),
            serde_json::to_vec(&json!({
                "messages": [{
                    "id": "extension-message",
                    "type": "message",
                    "role": "assistant",
                }]
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            project.join("index.json"),
            serde_json::to_vec(&json!({
                "conversations": [{
                    "id": "shared-session",
                    "name": "Shared native IDs",
                    "projectPath": "/workspace/codebuddy-ide",
                    "createdAt": IMPORTED_AT,
                }]
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            session.join("messages/extension-message.json"),
            serde_json::to_vec(&json!({
                "id": "extension-message",
                "role": "assistant",
                "content": extension_text,
                "createdAt": IMPORTED_AT,
            }))
            .unwrap(),
        )
        .unwrap();
    }

    fn scan(root: &Path) -> Vec<CodeBuddySourceBackedScan> {
        scan_codebuddy_source_backed_root(root, IMPORTED_AT.parse().unwrap()).unwrap()
    }

    fn documents(scans: &[CodeBuddySourceBackedScan]) -> BTreeMap<String, &LexicalDocument> {
        scans
            .iter()
            .flat_map(|scan| scan.pages.iter().flat_map(|page| page.documents.iter()))
            .map(|document| (document.source.schema_variant().to_owned(), document))
            .collect()
    }

    #[test]
    fn dual_format_cold_scan_emits_independent_full_body_exact_records() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("codebuddy");
        let cli_text = format!("cli exact head {} cli exact tail", "c".repeat(3_000));
        let extension_text = format!(
            "extension exact head {} extension exact tail",
            "e".repeat(3_000)
        );
        write_dual_store(&root, &cli_text, &extension_text);

        let scans = scan(&root);
        assert_eq!(scans.len(), 2);
        assert_ne!(
            scans[0].source.observation().source().identity(),
            scans[1].source.observation().source().identity(),
            "CLI and IDE stores with equal native project/session IDs must remain independent"
        );
        for scan in &scans {
            assert_eq!(scan.source.counts().complete_records, 1);
            assert_eq!(scan.source.counts().retained_records, 1);
            assert_eq!(scan.source.counts().indexed_documents, 1);
            assert!(scan.rejections.is_empty());
            assert!(scan.pages.iter().all(|page| page.documents.len() <= 64));
        }

        let documents = documents(&scans);
        let cli = documents.get(CODEBUDDY_CLI_SCHEMA_VARIANT).unwrap();
        assert_eq!(cli.body, cli_text);
        assert!(cli.body.ends_with("cli exact tail"));
        let NativeRecordCoordinate::Jsonl {
            native_event_key, ..
        } = cli.locator.coordinate()
        else {
            panic!("CLI record must use a JSONL range");
        };
        assert!(tagged_event_key_matches(
            native_event_key.as_ref(),
            CODEBUDDY_CLI_LOCATOR_TAG,
            "cli-message"
        ));
        let hydrated_cli = hydrate_codebuddy_source_backed_record(&root, &cli.locator).unwrap();
        assert_eq!(hydrated_cli.decoded_display_text, cli_text);
        assert_eq!(hydrated_cli.provider_bytes, cli_text.as_bytes());

        let extension = documents.get(CODEBUDDY_EXTENSION_SCHEMA_VARIANT).unwrap();
        assert_eq!(extension.body, extension_text);
        assert!(extension.body.ends_with("extension exact tail"));
        let (_, _, native_id) = structured_coordinate(extension.locator.coordinate()).unwrap();
        assert_eq!(native_id, "shared-project/shared-session:extension-message");
        let hydrated_extension =
            hydrate_codebuddy_source_backed_record(&root, &extension.locator).unwrap();
        assert_eq!(hydrated_extension.decoded_display_text, extension_text);
        assert_eq!(hydrated_extension.provider_bytes, extension_text.as_bytes());
    }

    #[test]
    fn dual_format_replacement_preserves_stable_ids_and_rejects_stale_locators() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("codebuddy");
        write_dual_store(&root, "CLI before replacement", "IDE before replacement");
        let before = scan(&root);
        let before_documents = documents(&before);
        let before_state = before_documents
            .iter()
            .map(|(shape, document)| {
                (
                    shape.clone(),
                    (
                        document.source.identity(),
                        document.session_id,
                        document.event_id,
                        document.locator.clone(),
                    ),
                )
            })
            .collect::<BTreeMap<_, _>>();
        drop(before_documents);

        let replacement = root.join("projects/shared-project/replacement.jsonl");
        fs::write(
            &replacement,
            format!(
                "{}\n",
                serde_json::to_string(&json!({
                    "id": "cli-message",
                    "type": "message",
                    "role": "user",
                    "content": "CLI after replacement with changed bytes",
                    "timestamp": IMPORTED_AT,
                    "sessionId": "shared-session",
                    "cwd": "/workspace/codebuddy-cli",
                }))
                .unwrap()
            ),
        )
        .unwrap();
        fs::rename(
            &replacement,
            root.join("projects/shared-project/shared-session.jsonl"),
        )
        .unwrap();
        fs::write(
            root.join("history/shared-project/shared-session/messages/extension-message.json"),
            serde_json::to_vec(&json!({
                "id": "extension-message",
                "role": "assistant",
                "content": "IDE after replacement with changed bytes",
                "createdAt": IMPORTED_AT,
            }))
            .unwrap(),
        )
        .unwrap();

        let after = scan(&root);
        assert_eq!(after.len(), 2, "both installed stores must remain selected");
        let after_documents = documents(&after);
        for (shape, document) in &after_documents {
            let (source_id, session_id, event_id, stale_locator) = before_state.get(shape).unwrap();
            assert_eq!(document.source.identity(), *source_id);
            assert_eq!(document.session_id, *session_id);
            assert_eq!(document.event_id, *event_id);
            assert!(
                hydrate_codebuddy_source_backed_record(&root, stale_locator).is_err(),
                "{shape} stale locator unexpectedly hydrated after replacement"
            );
            let hydrated =
                hydrate_codebuddy_source_backed_record(&root, &document.locator).unwrap();
            assert!(hydrated.decoded_display_text.contains("after replacement"));
        }
        for scan in &after {
            let prior = before
                .iter()
                .find(|candidate| {
                    candidate.source.observation().source().schema_variant()
                        == scan.source.observation().source().schema_variant()
                })
                .unwrap();
            assert_ne!(scan.source.content_digest(), prior.source.content_digest());
        }
    }

    #[test]
    fn compound_authority_codebuddy_rejects_missing_auxiliary_and_sibling_swap() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("codebuddy");
        write_dual_store(&root, "cli", "extension");
        let project_index = root.join("history/shared-project/index.json");
        fs::remove_file(&project_index).unwrap();
        let mut inventory = discover_sources(&root, &ProviderImportOptions::default()).unwrap();
        let authority = codebuddy_authority(&root).unwrap();
        let extension = inventory
            .sources
            .iter_mut()
            .find(|source| source.shape == CodeBuddySourceShape::Extension)
            .unwrap();
        bind_codebuddy_capability(extension, &authority).unwrap();
        fs::write(&project_index, br#"{"conversations":[]}"#).unwrap();
        assert!(revalidate_codebuddy_capability(extension).is_err());

        let temp = tempdir().unwrap();
        let root = temp.path().join("codebuddy");
        write_dual_store(&root, "cli", "extension");
        let mut inventory = discover_sources(&root, &ProviderImportOptions::default()).unwrap();
        let authority = codebuddy_authority(&root).unwrap();
        let extension = inventory
            .sources
            .iter_mut()
            .find(|source| source.shape == CodeBuddySourceShape::Extension)
            .unwrap();
        bind_codebuddy_capability(extension, &authority).unwrap();
        let message =
            root.join("history/shared-project/shared-session/messages/extension-message.json");
        let bytes = fs::read(&message).unwrap();
        fs::rename(&message, message.with_extension("retired")).unwrap();
        fs::write(&message, bytes).unwrap();
        assert!(revalidate_codebuddy_capability(extension).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn compound_authority_codebuddy_rejects_ancestor_swap_and_stale_locator() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("codebuddy");
        write_dual_store(&root, "cli before", "extension before");
        let scans = scan(&root);
        let stale_extension = documents(&scans)[CODEBUDDY_EXTENSION_SCHEMA_VARIANT]
            .locator
            .clone();

        let mut inventory = discover_sources(&root, &ProviderImportOptions::default()).unwrap();
        let authority = codebuddy_authority(&root).unwrap();
        for source in &mut inventory.sources {
            bind_codebuddy_capability(source, &authority).unwrap();
        }
        let retired = temp.path().join("retired-codebuddy");
        fs::rename(&root, &retired).unwrap();
        write_dual_store(&root, "cli after", "extension after");
        assert!(inventory
            .sources
            .iter()
            .all(|source| revalidate_codebuddy_capability(source).is_err()));
        assert!(hydrate_codebuddy_source_backed_record(&root, &stale_extension).is_err());
    }
}
pub(crate) mod registration {
    use chrono::{DateTime, Utc};
    use ctx_history_core::{CaptureProvider, HydratedProviderRecord, HydrationFailureKind};

    use super::{hydrate_codebuddy_source_backed_record, scan_codebuddy_source_backed_root};
    use crate::provider::source_backed::{
        captured_route_driver, executable_route, hydration_failure, provider_format_scope,
        route_capture_error, SourceBackedCoordinatorResult, SourceBackedProviderRegistry,
        SourceBackedRouteSelection, SourceBackedSelectorAuthority,
    };
    use crate::ProviderSource;

    pub(crate) fn register(
        registry: &mut SourceBackedProviderRegistry,
        source: ProviderSource,
        selection: SourceBackedRouteSelection,
    ) -> SourceBackedCoordinatorResult<()> {
        let root = source.path.clone();
        let capture_root = root.clone();
        let hydration_root = root;
        let driver = captured_route_driver(
            move |sink| {
                for scan in
                    scan_codebuddy_source_backed_root(&capture_root, DateTime::<Utc>::UNIX_EPOCH)
                        .map_err(route_capture_error)?
                {
                    sink.begin(scan.source.observation().source().clone())?;
                    for page in scan.pages {
                        for document in page.documents {
                            sink.document(document)?;
                        }
                    }
                    sink.certify(scan.source)?;
                }
                Ok(())
            },
            provider_format_scope(CaptureProvider::CodeBuddy, "codebuddy_history_json"),
            move |request| {
                let hydrated =
                    hydrate_codebuddy_source_backed_record(&hydration_root, request.locator())
                        .map_err(|error| {
                            hydration_failure(HydrationFailureKind::StaleRecordEvidence, error)
                        })?;
                Ok(HydratedProviderRecord {
                    event_id: request.event_id(),
                    provider_bytes: hydrated.provider_bytes,
                })
            },
        );
        registry.register(executable_route(
            source,
            selection,
            SourceBackedSelectorAuthority::DiscoveredWinner,
            driver,
        )?);
        Ok(())
    }
}
