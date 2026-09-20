use super::*;
use crate::conversation::{self, ConversationManifest};
use ctx_history_core::CoreRecord;
use ctx_history_provider_runtime::JsonlRecordRef;

impl<B> FxSessionsTreeAdapter<B> {
    pub(super) fn discover_conversation(
        &self,
        relative: &Path,
        authority: &Arc<ProviderSourceRoot>,
        inventory: &mut InventoryBuild,
    ) -> Result<bool, CaptureError> {
        let path = relative.join("session.json");
        let (opened, bytes) = match read_limited(authority, &path, LEGACY_MAX_BYTES) {
            Ok(value) => value,
            Err(error) if error.is_not_found() => return Ok(false),
            // Existing v3 authority may coexist with an unusable derived snapshot.
            Err(_) => return Ok(false),
        };
        let value: serde_json::Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) => return Ok(false),
        };
        if value
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            != Some(4)
        {
            return Ok(false);
        }
        let result = (|| {
            let manifest: ConversationManifest = serde_json::from_value(value)?;
            if manifest.schema_version != 4
                || safe_directory_name(relative) != Some(manifest.id.as_str())
            {
                return Err(CaptureError::InvalidPayload(
                    "fx conversation manifest identity mismatch".into(),
                ));
            }
            let source = self.source_key(&manifest.id)?;
            let events = relative.join("events.jsonl");
            let source_path = authority.named_path().join(&events);
            let events_opened = authority.open_file(&events)?;
            let observation =
                ctx_history_provider_runtime::observe_opened_file(&source_path, &events_opened)?;
            let mut leaf = ProviderJsonlLeaf::bind_observed(
                source.clone(),
                source_path,
                Arc::clone(authority),
                events,
                TypedKey::utf8(&manifest.id).map_err(contract)?,
                observation,
            )
            .with_exact_present_dependency(path.clone(), &opened)?;
            let artifacts = Arc::new(super::artifacts::SessionArtifacts::observe(
                Arc::clone(authority),
                relative,
            )?);
            leaf = artifacts.bind(leaf)?;
            let owner_path = relative.join("subagent/owner.json");
            let parent = match read_limited(authority, &owner_path, SIDECAR_MAX_BYTES) {
                Ok((owner, bytes)) => {
                    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
                    if value
                        .get("schema_version")
                        .and_then(serde_json::Value::as_u64)
                        != Some(1)
                    {
                        return Err(CaptureError::UnsupportedSchema("fx child owner".into()));
                    }
                    let parent = value
                        .get("parent_id")
                        .and_then(serde_json::Value::as_str)
                        .ok_or_else(|| {
                            CaptureError::InvalidPayload("fx child owner missing parent_id".into())
                        })?
                        .to_owned();
                    leaf = leaf.with_exact_present_dependency(owner_path, &owner)?;
                    let parent_source = self.source_key(&parent)?;
                    Some((parent, parent_source))
                }
                Err(error) if error.is_not_found() => {
                    leaf = leaf.with_exact_absent_dependency(owner_path)?;
                    None
                }
                Err(error) => return Err(error),
            };
            inventory.plans.insert(
                source,
                SessionPlan::Conversation {
                    source_path: leaf.source_path().to_owned(),
                    observation: leaf.observation().clone(),
                    logical_eof: None,
                    manifest,
                    parent,
                    artifacts,
                },
            );
            inventory.leaves.push(leaf);
            Ok::<_, CaptureError>(())
        })();
        if let Err(error) = result {
            let source = safe_directory_name(relative)
                .map(|id| self.source_key(id))
                .transpose()?;
            reject_path(
                authority,
                &path,
                &mut inventory.rejected,
                source,
                error.to_string(),
            )?;
        }
        Ok(true)
    }
}

pub(super) struct ConversationProjector<B: ProviderRuntimeBinding> {
    leaf: ProviderJsonlLeaf,
    manifest: ConversationManifest,
    parent: Option<(String, SourceKey)>,
    artifacts: Arc<super::artifacts::SessionArtifacts>,
    last_seq: u64,
    rejected: ctx_history_jsonl::JsonlRecordRejections,
    binding: std::marker::PhantomData<fn() -> B>,
}

impl<B: ProviderRuntimeBinding> ConversationProjector<B> {
    pub(super) fn new(
        leaf: ProviderJsonlLeaf,
        manifest: ConversationManifest,
        parent: Option<(String, SourceKey)>,
        artifacts: Arc<super::artifacts::SessionArtifacts>,
        checkpoint: Option<&TypedKey>,
    ) -> Result<Self, CaptureError> {
        let last_seq = match checkpoint {
            None => 0,
            Some(TypedKey::U64(seq)) => *seq,
            _ => {
                return Err(CaptureError::InvalidPayload(
                    "fx conversation checkpoint malformed".into(),
                ))
            }
        };
        let rejected = ctx_history_jsonl::JsonlRecordRejections::new(
            leaf.source().clone(),
            CaptureProvider::Fx,
            leaf.source_path().display().to_string(),
        );
        Ok(Self {
            leaf,
            manifest,
            parent,
            artifacts,
            last_seq,
            rejected,
            binding: std::marker::PhantomData,
        })
    }
}

impl<B: ProviderRuntimeBinding> JsonlFamilyProjector for ConversationProjector<B> {
    type Runtime = ProviderJsonlRuntime<B>;
    fn project(
        &mut self,
        row: JsonlRecordRef<'_>,
        _worker: &mut ProviderJsonlWorkerContext<B>,
        emit: &mut dyn FnMut(CoreRecord) -> Result<(), CaptureError>,
    ) -> Result<(), CaptureError> {
        let result = conversation::project(
            ProjectionBinding {
                source: self.leaf.source(),
                native_session_id: &self.manifest.id,
            },
            &self.manifest,
            self.parent.as_ref().map(|(id, source)| ProjectionBinding {
                source,
                native_session_id: id,
            }),
            row.bytes(),
            &mut |kind, name| self.artifacts.read(kind, name),
        );
        match result {
            Ok(record) => {
                let Some(TypedKey::U64(seq)) = record.native_event_id else {
                    return Err(CaptureError::SystemInvariant(
                        "fx conversation sequence missing",
                    ));
                };
                if seq <= self.last_seq {
                    self.rejected
                        .malformed(row, "fx conversation sequence is not increasing");
                    return Ok(());
                }
                self.last_seq = seq;
                emit(record)
            }
            Err(
                error @ (CaptureError::InvalidPayload(_)
                | CaptureError::UnsupportedSchema(_)
                | CaptureError::Json(_)),
            ) => {
                self.rejected.malformed(row, error.to_string());
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
    fn rejected_records(&self) -> u64 {
        self.rejected.count()
    }
    fn take_record_rejections(&mut self) -> ctx_history_jsonl::SourceBackedRecordRejectionDrafts {
        self.rejected.take_drafts()
    }
    fn provider_checkpoint(&self) -> Result<Option<TypedKey>, CaptureError> {
        Ok(Some(TypedKey::U64(self.last_seq)))
    }
}
