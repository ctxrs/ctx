use super::*;

pub(super) struct ClaudeOutputState {
    pub(super) source: OutputSourceIdentity,
    pub(super) progress: Option<ProOutputProgress>,
    pub(super) previous: Option<ParseCheckpoint>,
    pub(super) source_epoch: u64,
    pub(super) disposition: ProOutputSourceDisposition,
    pub(super) expected_source_epoch: Option<u64>,
    pub(super) expected_cursor: Option<OutputNativeCursor>,
    pub(super) enabled: bool,
}

pub(super) fn output_state(
    source: &DiscoveredClaudeSession,
    source_root: &Path,
    sink: &dyn ProOutputSink,
) -> ClaudeOutputState {
    let identity = OutputSourceIdentity {
        provider: CaptureProvider::Claude.as_str().to_owned(),
        namespace_id: source_root.display().to_string(),
        source_id: provider_path_identity(&source.canonical_path)
            .unwrap_or_else(|_| source.canonical_path.display().to_string()),
    };
    let progress = match sink.observe_source(&identity) {
        Ok(progress) => progress,
        Err(error) => {
            sink.mark_behind(error);
            return ClaudeOutputState {
                source: identity,
                progress: None,
                previous: None,
                source_epoch: 0,
                disposition: ProOutputSourceDisposition::NewSource,
                expected_source_epoch: None,
                expected_cursor: None,
                enabled: false,
            };
        }
    };
    let previous = progress.as_ref().and_then(|progress| {
        (progress.parser_revision == CLAUDE_OUTPUT_PARSER_REVISION
            && progress.materializer_revision == sink.materializer_revision())
        .then_some(progress.cursor.as_ref())
        .flatten()
        .filter(|cursor| cursor.version == CLAUDE_OUTPUT_CURSOR_VERSION)
        .and_then(|cursor| serde_json::from_slice::<ParseCheckpoint>(&cursor.payload).ok())
    });
    let resumable = progress.is_none() || previous.is_some();
    let source_epoch = progress.as_ref().map_or(0, |progress| {
        if resumable {
            progress.source_epoch
        } else {
            progress.source_epoch.saturating_add(1)
        }
    });
    ClaudeOutputState {
        source: identity,
        previous,
        source_epoch,
        disposition: if progress.is_none() {
            ProOutputSourceDisposition::NewSource
        } else if resumable {
            ProOutputSourceDisposition::AppendOrResume
        } else {
            ProOutputSourceDisposition::Rewrite
        },
        expected_source_epoch: progress.as_ref().map(|progress| progress.source_epoch),
        expected_cursor: progress
            .as_ref()
            .and_then(|progress| progress.cursor.clone()),
        progress,
        enabled: true,
    }
}

pub(super) fn output_is_aligned(
    core: Option<&ClaudeStoreCursor>,
    output: &ClaudeOutputState,
) -> bool {
    match (core, output.progress.as_ref(), output.previous.as_ref()) {
        (None, None, None) => true,
        (Some(core), Some(_), Some(output)) => {
            output.pro_revisions_match()
                && output.pro_observation_binding_matches()
                && core.checkpoint.core_frontier() == output.pro_frontier()
                && core.checkpoint.terminal == output.pro_terminal
        }
        _ => false,
    }
}

pub(super) fn copy_pro_lane(core: &mut ParseCheckpoint, output: &ParseCheckpoint) {
    core.pro_complete_offset = output.pro_complete_offset;
    core.pro_next_raw_ordinal = output.pro_next_raw_ordinal;
    core.pro_complete_record_chain_sha256 = output.pro_complete_record_chain_sha256;
    core.pro_boundary_proof_len = output.pro_boundary_proof_len;
    core.pro_boundary_proof_sha256 = output.pro_boundary_proof_sha256;
    core.pro_native_identity_chain_sha256 = output.pro_native_identity_chain_sha256;
    core.pro_native_identity_records = output.pro_native_identity_records;
    core.pro_appendable_boundary = output.pro_appendable_boundary;
    core.pro_initialized = output.pro_initialized;
    core.pro_terminal = output.pro_terminal;
    core.pro_observed_file_len = output.pro_observed_file_len;
    core.pro_observation_sha256 = output.pro_observation_sha256;
    core.pro_observation_binding_sha256 = output.pro_observation_binding_sha256;
    core.pro_parser_revision = output.pro_parser_revision;
    core.pro_policy_revision = output.pro_policy_revision;
}

pub(super) fn materialize_output_page(
    source: &DiscoveredClaudeSession,
    sink: &dyn ProOutputSink,
    state: &mut ClaudeOutputState,
    page: ClaudeNativeProOutputPage,
    checkpoint: ParseCheckpoint,
) {
    if !state.enabled {
        return;
    }
    if let (Some(progress), Some(previous)) = (&state.progress, &state.previous) {
        if page.expected_frontier != previous.pro_frontier()
            && state.disposition == ProOutputSourceDisposition::AppendOrResume
        {
            state.source_epoch = progress.source_epoch.saturating_add(1);
            state.disposition = ProOutputSourceDisposition::Rewrite;
        }
    }
    let next_cursor = match serde_json::to_vec(&checkpoint) {
        Ok(payload) => OutputNativeCursor {
            version: CLAUDE_OUTPUT_CURSOR_VERSION,
            payload,
        },
        Err(error) => {
            state.enabled = false;
            sink.mark_behind(ProOutputSinkError::new(
                "claude_nativepath_output_cursor",
                error.to_string(),
            ));
            return;
        }
    };
    let materialization = ProOutputMaterializationPage {
        inventory_generation: sink.inventory_generation(),
        source: state.source.clone(),
        source_epoch: state.source_epoch,
        observed_revision: source_revision(source, None),
        parser_revision: CLAUDE_OUTPUT_PARSER_REVISION.to_owned(),
        materializer_revision: sink.materializer_revision().to_owned(),
        disposition: state.disposition,
        expected_prior_source_epoch: state.expected_source_epoch,
        expected_prior_cursor: state.expected_cursor.clone(),
        next_safe_cursor: next_cursor.clone(),
        terminal: page.terminal,
        observations: page.outputs,
    };
    match sink.materialize_page(materialization) {
        Ok(result) => {
            state.expected_source_epoch = Some(result.source_epoch);
            state.expected_cursor = Some(result.committed_cursor);
            state.disposition = ProOutputSourceDisposition::AppendOrResume;
            state.previous = Some(checkpoint);
        }
        Err(error) => {
            state.enabled = false;
            sink.mark_behind(error);
        }
    }
}

pub(super) fn replay_source_outputs(
    source: &DiscoveredClaudeSession,
    source_root: &Path,
    sink: &dyn ProOutputSink,
) {
    let mut state = output_state(source, source_root, sink);
    if !state.enabled {
        return;
    }
    let previous = state.previous.clone();
    let mut scanner = match ClaudeNativeScanner::new(
        source.clone(),
        previous.as_ref(),
        ClaudeNativeProfile::ProReplayOnly,
    ) {
        Ok(scanner) => scanner,
        Err(error) => {
            sink.mark_behind(ProOutputSinkError::new(
                "claude_nativepath_output_replay",
                error.to_string(),
            ));
            return;
        }
    };
    loop {
        let page = match scanner.next_page() {
            Ok(Some(ClaudeNativeOwnedPage::Pro(page))) => page,
            Ok(Some(ClaudeNativeOwnedPage::Core(_))) => {
                sink.mark_behind(ProOutputSinkError::new(
                    "claude_nativepath_output_replay",
                    "Pro replay emitted a Core page",
                ));
                return;
            }
            Ok(None) => break,
            Err(error) => {
                sink.mark_behind(ProOutputSinkError::new(
                    "claude_nativepath_output_replay",
                    error.to_string(),
                ));
                return;
            }
        };
        let checkpoint = scanner.checkpoint_at(&page.next_safe_frontier, page.terminal);
        materialize_output_page(source, sink, &mut state, *page, checkpoint);
        if !state.enabled {
            return;
        }
    }
    if let Err(error) = scanner.finish() {
        sink.mark_behind(ProOutputSinkError::new(
            "claude_nativepath_output_replay",
            error.to_string(),
        ));
    }
}
