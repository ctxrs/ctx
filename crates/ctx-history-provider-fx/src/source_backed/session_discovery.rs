use super::*;

impl<B> FxSessionsTreeAdapter<B> {
    pub(super) fn discover_session(
        &self,
        route_root: &Path,
        session_relative: PathBuf,
        authority: Arc<ProviderSourceRoot>,
        inventory: &mut InventoryBuild,
    ) -> Result<(), CaptureError> {
        if self.discover_conversation(&session_relative, &authority, inventory)? {
            return Ok(());
        }
        let InventoryBuild {
            leaves,
            rejected,
            pending,
            plans,
            metadata_entries_remaining,
        } = inventory;
        let authority_marker = session_relative.join("authority.json");
        let legacy_snapshot = session_relative.join("session.json");
        let events = session_relative.join("events.jsonl");
        let authority_pending = session_relative.join("authority.pending.json");
        let commit_pending = session_relative.join("commit.pending.json");
        let provisional = safe_directory_name(&session_relative)
            .map(|name| self.source_key(name))
            .transpose()?;
        // Pending sidecars are authoritative transition evidence. Admit one before
        // touching any stable sidecar: a writer may have removed/replaced the stable
        // triplet already, and that state must retain a prior durable source rather than
        // look like a deletion or a malformed ordinary session.
        let authority_pending_exists = match pending_exists(&authority, &authority_pending) {
            Ok(value) => value,
            Err(error) => {
                return reject_path(
                    &authority,
                    &authority_pending,
                    rejected,
                    provisional,
                    error.to_string(),
                )
            }
        };
        let commit_pending_exists = match pending_exists(&authority, &commit_pending) {
            Ok(value) => value,
            Err(error) => {
                return reject_path(
                    &authority,
                    &commit_pending,
                    rejected,
                    provisional,
                    error.to_string(),
                )
            }
        };
        let pending_path = if authority_pending_exists {
            Some(authority_pending.clone())
        } else if commit_pending_exists {
            Some(commit_pending.clone())
        } else {
            None
        };
        if let Some(pending_path) = pending_path {
            let opened = match authority.open_file(&pending_path) {
                Ok(opened) => opened,
                Err(error) => {
                    return reject_path(
                        &authority,
                        &pending_path,
                        rejected,
                        provisional,
                        error.to_string(),
                    )
                }
            };
            let source_path = authority.named_path().join(&pending_path);
            pending.push(JsonlFamilyPendingLeaf::bind_observed(
                source_path.clone(),
                pending_path,
                ctx_history_provider_runtime::observe_opened_file(&source_path, &opened)?,
                TypedKey::utf8("fx session authority transition is pending").map_err(contract)?,
                provisional,
            ));
            return Ok(());
        }
        let marker = match read_limited(&authority, &authority_marker, SIDECAR_MAX_BYTES) {
            Ok(value) => Some(value),
            Err(error) if error.is_not_found() => None,
            Err(error) => {
                return reject_path(
                    &authority,
                    &authority_marker,
                    rejected,
                    provisional,
                    error.to_string(),
                )
            }
        };
        if let Some((opened_authority, authority_bytes)) = marker {
            let parsed_authority = match decode_authority(&authority_bytes, ReplayLimits::default())
            {
                Ok(value) => value,
                Err(error) => {
                    return self.reject_events(&authority, &events, rejected, provisional, error);
                }
            };
            let native_session_id = parsed_authority.session_id.clone();
            let source = self.source_key(&native_session_id)?;
            if safe_directory_name(&session_relative) != Some(native_session_id.as_str()) {
                return reject_path(
                    &authority,
                    &events,
                    rejected,
                    provisional.or(Some(source)),
                    "fx authority session ID does not match its directory name".to_owned(),
                );
            }
            let events_opened = match authority.open_file(&events) {
                Ok(value) => value,
                Err(error) => {
                    return reject_path(
                        &authority,
                        &events,
                        rejected,
                        Some(source),
                        error.to_string(),
                    )
                }
            };
            let first_read = match events_opened.read_exact_range(
                0,
                usize::try_from(
                    events_opened
                        .len()
                        .min(crate::EVENT_FRAME_MAX_BYTES as u64 + 1),
                )
                .map_err(|_| {
                    CaptureError::SystemInvariant("fx first frame length exceeds usize")
                })?,
                crate::EVENT_FRAME_MAX_BYTES + 1,
            ) {
                Ok(value) => value,
                Err(error) => {
                    return reject_path(
                        &authority,
                        &events,
                        rejected,
                        Some(source),
                        error.to_string(),
                    )
                }
            };
            let Some(newline) = first_read.iter().position(|byte| *byte == b'\n') else {
                return reject_path(
                    &authority,
                    &events,
                    rejected,
                    Some(source),
                    "fx event log has no bounded complete first frame".to_owned(),
                );
            };
            let first =
                match decode_first_event_binding(&first_read[..newline], ReplayLimits::default()) {
                    Ok(value) => value,
                    Err(error) => {
                        return self.reject_events(
                            &authority,
                            &events,
                            rejected,
                            Some(source),
                            error,
                        )
                    }
                };
            if first.native_session_id != native_session_id {
                return self.reject_events(
                    &authority,
                    &events,
                    rejected,
                    Some(source),
                    FxProviderError::InvalidAuthority(
                        "first event session does not match authority",
                    ),
                );
            }
            let watermark_path =
                session_relative.join(format!("commit.{}.json", first.log_generation));
            let (opened_watermark, watermark_bytes) =
                match read_limited(&authority, &watermark_path, SIDECAR_MAX_BYTES) {
                    Ok(value) => value,
                    Err(error) => {
                        return reject_path(
                            &authority,
                            &events,
                            rejected,
                            Some(source),
                            format!("fx commit sidecar is unavailable: {error}"),
                        )
                    }
                };
            let watermark = match decode_watermark(&watermark_bytes, ReplayLimits::default()) {
                Ok(value) => value,
                Err(error) => {
                    return self.reject_events(&authority, &events, rejected, Some(source), error);
                }
            };
            if watermark.session_id != native_session_id
                || watermark.log_generation != first.log_generation
                || watermark.through_seq == 0
                || watermark.through_event_log_bytes > events_opened.len()
            {
                return self.reject_events(
                    &authority,
                    &events,
                    rejected,
                    Some(source),
                    FxProviderError::InvalidAuthority("commit session does not match authority"),
                );
            }
            let events_path = authority.named_path().join(&events);
            let observation =
                ctx_history_provider_runtime::observe_opened_file(&events_path, &events_opened)?;
            let leaf = ProviderJsonlLeaf::bind_observed(
                source.clone(),
                events_path,
                Arc::clone(&authority),
                events,
                TypedKey::utf8(&native_session_id).map_err(contract)?,
                observation,
            )
            .with_logical_eof(watermark.through_event_log_bytes)?
            .with_exact_present_dependency(authority_marker, &opened_authority)?
            .with_exact_present_dependency(watermark_path, &opened_watermark)?
            .with_exact_absent_dependency(authority_pending)?
            .with_exact_absent_dependency(commit_pending)?;
            plans.insert(
                source,
                SessionPlan::V3 {
                    source_path: leaf.source_path().to_path_buf(),
                    observation: leaf.observation().clone(),
                    logical_eof: leaf.logical_eof(),
                    authority: parsed_authority,
                    watermark,
                },
            );
            leaves.push(leaf);
            return Ok(());
        }

        let markerless_v3 = match has_markerless_v3_evidence(
            &authority,
            &session_relative,
            metadata_entries_remaining,
        ) {
            Ok(value) => value,
            Err(error) => {
                return reject_path(
                    &authority,
                    &events,
                    rejected,
                    provisional,
                    error.to_string(),
                )
            }
        };
        if markerless_v3 {
            return reject_path(
                &authority,
                &events,
                rejected,
                provisional,
                "fx markerless v3 evidence has no authority marker".to_owned(),
            );
        }
        let (_opened_snapshot, snapshot) =
            match read_limited(&authority, &legacy_snapshot, LEGACY_MAX_BYTES) {
                Ok(value) => value,
                Err(error) if error.is_not_found() => return Ok(()),
                Err(error) => {
                    return reject_path(
                        &authority,
                        &legacy_snapshot,
                        rejected,
                        provisional,
                        error.to_string(),
                    )
                }
            };
        let defaults = LegacyDefaults {
            source_root: route_root.display().to_string(),
            preferences: crate::SessionPreferences {
                provider: crate::ProviderId::Gateway,
                model: "fx-legacy".to_owned(),
                effort: "auto".to_owned(),
                fast_mode: false,
            },
        };
        let legacy = match replay_legacy_snapshot(&snapshot, &defaults, ReplayLimits::default()) {
            Ok(value) => value,
            Err(error) => {
                return self.reject_snapshot(
                    &authority,
                    &legacy_snapshot,
                    rejected,
                    provisional,
                    error,
                );
            }
        };
        let source = self.source_key(&legacy.state.id)?;
        if safe_directory_name(&session_relative) != Some(legacy.state.id.as_str()) {
            return self.reject_snapshot(
                &authority,
                &legacy_snapshot,
                rejected,
                provisional.or(Some(source)),
                FxProviderError::InvalidLegacy(
                    "legacy session ID does not match its directory name",
                ),
            );
        }
        let snapshot_path = authority.named_path().join(&legacy_snapshot);
        let leaf = ProviderJsonlLeaf::observe_whole_record(
            source.clone(),
            snapshot_path,
            authority,
            legacy_snapshot,
            TypedKey::utf8(&legacy.state.id).map_err(contract)?,
        )?
        .with_exact_absent_dependency(authority_marker)?
        .with_exact_absent_dependency(authority_pending)?
        .with_exact_absent_dependency(commit_pending)?;
        plans.insert(
            source,
            SessionPlan::Legacy {
                source_path: leaf.source_path().to_path_buf(),
                observation: leaf.observation().clone(),
                logical_eof: leaf.logical_eof(),
                defaults,
            },
        );
        leaves.push(leaf);
        Ok(())
    }
}
