use super::*;

pub(super) fn plan_read(
    source: &DiscoveredClaudeSession,
    previous: Option<&ParseCheckpoint>,
    file: &mut File,
    stats: &mut ParseStats,
) -> Result<ReadPlan, ClaudeNativePathError> {
    let Some(previous) = previous else {
        return Ok(full_read_plan(ChangeSignal::Fresh));
    };
    if !previous.core_revisions_match() || !previous.core_observation_binding_matches() {
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
    let previous_route_exists = !same_route
        && std::path::absolute(&previous.canonical_route)
            .ok()
            .and_then(|path| crate::common::io::open_provider_source_file(&path).ok())
            .is_some();
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

    let selected = previous.core_frontier();
    if source.fingerprint.len < previous.observed_file_len
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
    let exact_observation = current_observation_sha256 == previous.observation_sha256
        && source.fingerprint.len == previous.observed_file_len
        && (!previous.terminal || selected.complete_offset == source.fingerprint.len);
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
    }
}
