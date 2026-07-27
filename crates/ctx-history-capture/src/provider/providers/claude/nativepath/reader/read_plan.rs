use super::*;

pub(super) fn plan_read(
    source: &DiscoveredClaudeSession,
    previous: Option<&ParseCheckpoint>,
    profile: ClaudeNativeProfile,
    file: &mut File,
    stats: &mut ParseStats,
) -> Result<ReadPlan, ClaudeNativePathError> {
    let Some(previous) = previous else {
        return Ok(full_read_plan(ChangeSignal::Fresh));
    };
    let selected_checkpoint_matches = match profile {
        ClaudeNativeProfile::CoreOnly => {
            previous.core_revisions_match() && previous.core_observation_binding_matches()
        }
        ClaudeNativeProfile::CoreAndPro => {
            previous.core_revisions_match()
                && previous.pro_revisions_match()
                && previous.core_observation_binding_matches()
                && previous.pro_observation_binding_matches()
        }
        ClaudeNativeProfile::ProReplayOnly => {
            previous.pro_revisions_match() && previous.pro_observation_binding_matches()
        }
    };
    if !selected_checkpoint_matches {
        return Ok(full_read_plan(ChangeSignal::Reparse));
    }
    let same_route = previous.canonical_route == source.canonical_path;
    if previous.session_key != source.key {
        return Ok(full_read_plan(if same_route {
            ChangeSignal::Replacement
        } else {
            ChangeSignal::Fresh
        }));
    }
    let same_physical = match (
        previous.physical_file_id,
        source.fingerprint.physical_file_id,
    ) {
        (Some(previous), Some(current)) => previous == current,
        _ => same_route,
    };
    let previous_route_exists =
        !same_route && std::fs::symlink_metadata(&previous.canonical_route).is_ok();
    if same_route && !same_physical {
        return Ok(full_read_plan(ChangeSignal::Replacement));
    }
    if !same_route && !same_physical {
        return Ok(full_read_plan(if previous_route_exists {
            ChangeSignal::LiveCopy
        } else {
            ChangeSignal::Replacement
        }));
    }

    let selected = match profile {
        ClaudeNativeProfile::CoreOnly => previous.core_frontier(),
        ClaudeNativeProfile::ProReplayOnly => previous.pro_frontier(),
        ClaudeNativeProfile::CoreAndPro => {
            let core = previous.core_frontier();
            let pro = previous.pro_frontier();
            if core != pro || previous.terminal != previous.pro_terminal {
                return Err(ClaudeNativePathError::InvalidCheckpoint {
                    reason:
                        "CoreAndPro requires aligned Core/Pro frontiers; replay Pro independently first"
                            .to_owned(),
                });
            }
            core
        }
    };
    let selected_terminal = match profile {
        ClaudeNativeProfile::CoreOnly => previous.terminal,
        ClaudeNativeProfile::CoreAndPro => previous.terminal && previous.pro_terminal,
        ClaudeNativeProfile::ProReplayOnly => previous.pro_terminal,
    };
    let selected_observed_file_len = match profile {
        ClaudeNativeProfile::CoreOnly => previous.observed_file_len,
        ClaudeNativeProfile::CoreAndPro => previous
            .observed_file_len
            .max(previous.pro_observed_file_len),
        ClaudeNativeProfile::ProReplayOnly => previous.pro_observed_file_len,
    };
    if source.fingerprint.len < selected_observed_file_len
        || source.fingerprint.len < selected.complete_offset
    {
        return Ok(full_read_plan(ChangeSignal::Truncation));
    }
    let verified_prefix = if selected.appendable_boundary {
        verify_committed_prefix(file, &selected, &source.path, stats)?
    } else {
        None
    };
    if verified_prefix.is_none() {
        return Ok(full_read_plan(if same_route {
            ChangeSignal::Rewrite
        } else if previous_route_exists {
            ChangeSignal::LiveCopy
        } else {
            ChangeSignal::Relocation
        }));
    }
    let boundary_window = verified_prefix.unwrap_or_default();
    let current_observation_sha256 = source.fingerprint.observation_sha256();
    let selected_observation_matches = match profile {
        ClaudeNativeProfile::CoreOnly => {
            current_observation_sha256 == previous.observation_sha256
                && source.fingerprint.len == previous.observed_file_len
        }
        ClaudeNativeProfile::CoreAndPro => {
            current_observation_sha256 == previous.observation_sha256
                && source.fingerprint.len == previous.observed_file_len
                && current_observation_sha256 == previous.pro_observation_sha256
                && source.fingerprint.len == previous.pro_observed_file_len
        }
        ClaudeNativeProfile::ProReplayOnly => {
            current_observation_sha256 == previous.pro_observation_sha256
                && source.fingerprint.len == previous.pro_observed_file_len
        }
    };
    let exact_observation = selected_observation_matches
        && (!selected_terminal || selected.complete_offset == source.fingerprint.len)
        && match profile {
            ClaudeNativeProfile::CoreOnly => true,
            ClaudeNativeProfile::CoreAndPro => previous.pro_initialized && selected_terminal,
            ClaudeNativeProfile::ProReplayOnly => previous.pro_initialized,
        };
    if exact_observation {
        return Ok(ReadPlan {
            change: if same_route {
                ChangeSignal::Unchanged
            } else {
                ChangeSignal::Relocation
            },
            parse: false,
            frontier: selected,
            boundary_window,
        });
    }
    if source.fingerprint.len >= selected.complete_offset {
        return Ok(ReadPlan {
            change: if same_route {
                ChangeSignal::Append
            } else if previous_route_exists {
                ChangeSignal::LiveCopy
            } else {
                ChangeSignal::Relocation
            },
            parse: true,
            frontier: selected,
            boundary_window,
        });
    }
    Ok(full_read_plan(if same_route {
        ChangeSignal::Rewrite
    } else if previous_route_exists {
        ChangeSignal::LiveCopy
    } else {
        ChangeSignal::Relocation
    }))
}

fn full_read_plan(change: ChangeSignal) -> ReadPlan {
    ReadPlan {
        change,
        parse: true,
        frontier: initial_frontier(),
        boundary_window: BoundaryWindow::default(),
    }
}

pub(super) fn initial_frontier() -> ClaudeNativeFrontier {
    ClaudeNativeFrontier {
        complete_offset: 0,
        next_raw_ordinal: 0,
        complete_record_chain_sha256: initial_record_chain(),
        boundary_proof_len: 0,
        boundary_proof_sha256: boundary_proof_hash(&[]),
        native_identity_chain_sha256: initial_identity_chain(),
        native_identity_records: 0,
        appendable_boundary: true,
    }
}

pub(super) fn lifecycle_from_change(change: ChangeSignal) -> ClaudeSourceLifecycle {
    match change {
        ChangeSignal::Fresh => ClaudeSourceLifecycle::New,
        ChangeSignal::Unchanged => ClaudeSourceLifecycle::Replay,
        ChangeSignal::Append => ClaudeSourceLifecycle::Append,
        ChangeSignal::Rewrite | ChangeSignal::Reparse => ClaudeSourceLifecycle::Rewrite,
        ChangeSignal::Truncation => ClaudeSourceLifecycle::Rewind,
        ChangeSignal::Replacement => ClaudeSourceLifecycle::Replacement,
        ChangeSignal::Relocation => ClaudeSourceLifecycle::Move,
        ChangeSignal::LiveCopy => ClaudeSourceLifecycle::Copy,
        ChangeSignal::ConflictingLiveCopy => ClaudeSourceLifecycle::Ambiguous,
    }
}

pub(super) fn refine_change_signal(
    signal: ChangeSignal,
    previous: Option<&ParseCheckpoint>,
    current: &ParseCheckpoint,
) -> ChangeSignal {
    let Some(previous) = previous else {
        return signal;
    };
    if signal == ChangeSignal::LiveCopy
        && (previous.complete_offset != current.complete_offset
            || previous.next_raw_ordinal != current.next_raw_ordinal
            || previous.complete_record_chain_sha256 != current.complete_record_chain_sha256)
    {
        ChangeSignal::ConflictingLiveCopy
    } else if signal == ChangeSignal::Replacement
        && previous.canonical_route != current.canonical_route
        && previous.session_key == current.session_key
        && previous.complete_offset == current.complete_offset
        && previous.next_raw_ordinal == current.next_raw_ordinal
        && previous.complete_record_chain_sha256 == current.complete_record_chain_sha256
    {
        ChangeSignal::Relocation
    } else {
        signal
    }
}

pub(super) fn empty_parsed_record() -> ParsedClaudeRecord {
    ParsedClaudeRecord {
        result: Default::default(),
        preallocation_exclusion: false,
        native_record_id: None,
        session_id: None,
        timestamp: None,
        cwd: None,
        version: None,
        git_branch: None,
        rows: Vec::new(),
        outputs: Vec::new(),
    }
}

pub(super) fn build_output_observations(
    source: &DiscoveredClaudeSession,
    locator: &ClaudePhysicalLocator,
    parsed: ParsedClaudeRecord,
) -> Vec<ProOutputObservation> {
    let provider_session_id = source.key.provider_session_id();
    let root_session_id = source.key.root_session_id.clone();
    let parent_session_id = source.key.parent_provider_session_id().map(str::to_owned);
    let occurred_at_unix_ms = parsed
        .timestamp
        .as_deref()
        .and_then(|timestamp| timestamp.parse::<DateTime<Utc>>().ok())
        .map(|timestamp| timestamp.timestamp_millis());
    let native_record_id = parsed
        .native_record_id
        .or_else(|| Some(format!("line-{}", locator.line_number)));
    parsed
        .outputs
        .into_iter()
        .map(|output| {
            let mut payload = Vec::with_capacity(20);
            payload.extend_from_slice(&0_u32.to_be_bytes());
            payload.extend_from_slice(&locator.byte_start.to_be_bytes());
            payload.extend_from_slice(&locator.byte_end_exclusive.to_be_bytes());
            let unit_key = if output.subrecord_index == 0 {
                format!("line-{}:output", locator.line_number)
            } else {
                format!(
                    "line-{}:output-{}",
                    locator.line_number, output.subrecord_index
                )
            };
            ProOutputObservation {
                kind: OutputObservationKind::Tool,
                coordinate: OutputNativeCoordinate {
                    unit_key,
                    native_sequence: locator.line_number.saturating_sub(1),
                    native_record_id: native_record_id.clone(),
                    source_record_ordinal: Some(locator.line_number.saturating_sub(1)),
                    source_record_subrecord_index: Some(output.subrecord_index),
                    byte_start: Some(locator.byte_start),
                    byte_end_exclusive: Some(locator.byte_end_exclusive),
                },
                occurred_at_unix_ms,
                associations: OutputAssociations {
                    direct_session_id: provider_session_id.clone(),
                    root_session_id: root_session_id.clone(),
                    parent_session_id: parent_session_id.clone(),
                    provider_session_id: Some(provider_session_id.clone()),
                    agent_id: source.key.agent_id.clone(),
                    repository: None,
                },
                call_id: output.call_id,
                command: None,
                outcome: output.outcome,
                locator: OutputSourceLocator {
                    version: 1,
                    kind: CLAUDE_OUTPUT_LOCATOR_KIND.to_owned(),
                    payload,
                },
                content: output.content.unwrap_or_default(),
            }
        })
        .collect()
}
