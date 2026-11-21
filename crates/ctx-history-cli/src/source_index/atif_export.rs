//! ATIF v1.8 single-session (`ctx show session --format atif`) and multi-session
//! (`ctx archive atif`) export over a pinned `VerifiedIndex` generation.

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, bail, Context, Result};
use ctx_history_capture::atif::{
    export_session_to_trajectory, export_trajectory_json_pretty, safe_trajectory_filename,
    AtifExportOptions, AtifExportSession, Trajectory, MAX_SUBAGENT_NESTING_DEPTH,
};
use ctx_history_core::{
    AgentScope as CoreAgentScope, CaptureProvider, CoreRecord, ProviderNativeSessionRelationship,
};
use ctx_history_index::{
    CoreEventRangeDirection, CoreEventRangeFilters, CoreEventRangeSelection, SessionRecord,
    VerifiedIndex, DEFAULT_CORE_EVENT_PAGE_BUDGET,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{transcript::TranscriptOutput, ui::Ui};

use super::{shared::open_index, show::SessionStreamResult};

const EXPORT_PAGE_SIZE: usize = 128;

/// Request parameters for batch ATIF v1.8 export (`ctx archive atif`).
#[derive(Debug, Clone)]
pub struct ArchiveAtifRequest {
    pub out_dir: PathBuf,
    pub sessions: Vec<String>,
    pub providers: Vec<CaptureProvider>,
    pub workspace: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub include_subagents: bool,
}

/// Summary of one exported ATIF v1.8 file.
#[derive(Debug, Clone)]
pub struct ArchiveAtifExportedFile {
    pub path: PathBuf,
    pub filename: String,
    pub ctx_session_id: String,
    pub provider: String,
    pub provider_session_id: Option<String>,
    pub steps: usize,
    pub subagents: usize,
}

/// Machine-readable receipt returned by [`export_archive_atif`].
#[derive(Debug, Clone)]
pub struct ArchiveAtifReceipt {
    pub schema_version: u32,
    pub operation: &'static str,
    pub atif_schema_version: &'static str,
    pub generation_id: String,
    pub output_dir: PathBuf,
    pub sessions_exported: usize,
    pub subagents_embedded: usize,
    pub steps_exported: usize,
    pub files: Vec<ArchiveAtifExportedFile>,
}

impl ArchiveAtifReceipt {
    pub fn to_json(&self) -> Value {
        let files: Vec<Value> = self
            .files
            .iter()
            .map(|file| {
                json!({
                    "path": file.path,
                    "filename": file.filename,
                    "ctx_session_id": file.ctx_session_id,
                    "provider": file.provider,
                    "provider_session_id": file.provider_session_id,
                    "steps": file.steps,
                    "subagents": file.subagents,
                })
            })
            .collect();
        json!({
            "schema_version": self.schema_version,
            "operation": self.operation,
            "atif_schema_version": self.atif_schema_version,
            "generation_id": self.generation_id,
            "output_dir": self.output_dir,
            "sessions_exported": self.sessions_exported,
            "subagents_embedded": self.subagents_embedded,
            "steps_exported": self.steps_exported,
            "files": files,
        })
    }
}

/// Exports one resolved session (plus embedded subagent sessions) as ATIF v1.8
/// JSON to stdout or `--out <file>`.
#[allow(clippy::too_many_arguments)]
pub(super) fn export_cli_session_atif(
    data_root: &Path,
    selector: Option<String>,
    provider_session_id: Option<String>,
    provider: Option<CaptureProvider>,
    provider_key: Option<String>,
    source_id: Option<String>,
    max_events: Option<usize>,
    out: Option<PathBuf>,
    ui: &mut Ui,
) -> Result<SessionStreamResult> {
    if max_events == Some(0) {
        bail!("--max-events must be positive");
    }
    let index = open_index(data_root)?;
    let session = ctx_history_read_application::resolve_show_session(
        &index,
        selector.as_deref(),
        provider_session_id.as_deref(),
        provider,
        provider_key.as_deref(),
        source_id.as_deref(),
    )?;

    let mut visited = HashSet::new();
    let export_session =
        load_export_session_tree(&index, &session, max_events, true, 0, &mut visited)?;
    let total_records = count_session_records(&export_session);
    let trajectory = export_session_to_trajectory(
        &export_session,
        AtifExportOptions {
            include_subagents: true,
        },
    )?;
    let rendered = export_trajectory_json_pretty(&trajectory)?;
    let bytes = rendered.as_bytes();

    let mut output = TranscriptOutput::create(out, ui.stdout_writer())?;
    output.write_all(bytes)?;
    output.flush()?;
    output.finish()?;

    Ok(SessionStreamResult {
        events_returned: total_records,
        content_bytes: bytes.len(),
        output_bytes: bytes.len(),
    })
}

/// Exports matching sessions from one pinned generation as validated ATIF v1.8
/// `.atif.json` files inside `request.out_dir`.
pub fn export_archive_atif(
    request: &ArchiveAtifRequest,
    data_root: &Path,
) -> Result<ArchiveAtifReceipt> {
    if request.out_dir.exists() && !request.out_dir.is_dir() {
        bail!(
            "ATIF export output path must be a directory: {}",
            request.out_dir.display()
        );
    }

    let filters = CoreEventRangeFilters {
        providers: request
            .providers
            .iter()
            .map(|provider| provider.as_str().to_owned())
            .collect(),
        workspace: request.workspace.clone(),
        direction: CoreEventRangeDirection::Ascending,
        ..CoreEventRangeFilters::default()
    };
    let has_range_filters = !filters.providers.is_empty()
        || filters.workspace.is_some()
        || request.since.is_some()
        || request.until.is_some();
    let selection = ctx_history_read_application::event_range_selection(
        request.since.as_deref(),
        request.until.as_deref(),
        filters,
    )?;

    let index = open_index(data_root)?;
    let generation_id = index.generation_id().to_owned();

    let mut explicit_sessions: BTreeMap<Uuid, SessionRecord> = BTreeMap::new();
    for selector in &request.sessions {
        let resolved = ctx_history_read_application::resolve_show_session(
            &index,
            Some(selector.as_str()),
            None,
            None,
            None,
            None,
        )?;
        explicit_sessions.insert(resolved.session_id.as_uuid(), resolved);
    }

    let selected_ids: BTreeSet<Uuid> = if explicit_sessions.is_empty() {
        collect_matching_session_ids(&index, &selection)?
    } else if has_range_filters {
        let filtered_ids = collect_matching_session_ids(&index, &selection)?;
        explicit_sessions
            .keys()
            .copied()
            .filter(|id| filtered_ids.contains(id))
            .collect()
    } else {
        explicit_sessions.keys().copied().collect()
    };

    let mut selected_records: BTreeMap<Uuid, SessionRecord> = BTreeMap::new();
    for session_id in &selected_ids {
        let record = if let Some(existing) = explicit_sessions.get(session_id) {
            existing.clone()
        } else if let Some(found) = index.session_by_id(*session_id)? {
            found
        } else {
            continue;
        };
        selected_records.insert(*session_id, record);
    }

    // When embedding subagents, avoid emitting duplicate standalone files for
    // child sessions whose ancestor is already included in this export batch.
    let mut roots: Vec<SessionRecord> = Vec::with_capacity(selected_records.len());
    for session in selected_records.values() {
        if request.include_subagents
            && has_selected_embedding_ancestor(&index, session, &selected_records)?
        {
            continue;
        }
        roots.push(session.clone());
    }
    roots.sort_by(|left, right| {
        left.first_occurred_at_unix_ms
            .cmp(&right.first_occurred_at_unix_ms)
            .then_with(|| left.session_id.as_uuid().cmp(&right.session_id.as_uuid()))
    });

    fs::create_dir_all(&request.out_dir)
        .with_context(|| format!("create ATIF output directory {}", request.out_dir.display()))?;

    let mut files = Vec::with_capacity(roots.len());
    let mut subagents_embedded = 0_usize;
    let mut steps_exported = 0_usize;

    for root in roots {
        let mut visited = HashSet::new();
        let export_session = load_export_session_tree(
            &index,
            &root,
            None,
            request.include_subagents,
            0,
            &mut visited,
        )?;
        let trajectory = export_session_to_trajectory(
            &export_session,
            AtifExportOptions {
                include_subagents: request.include_subagents,
            },
        )?;
        let rendered = export_trajectory_json_pretty(&trajectory)?;

        let provider = parse_capture_provider(&root.provider);
        let filename = safe_trajectory_filename(provider, root.session_id.as_uuid());
        let dest_path = request.out_dir.join(&filename);
        write_file_atomically(&dest_path, rendered.as_bytes())?;

        let subagent_count = count_embedded_subagents(&trajectory);
        let step_count = trajectory.steps.len();
        subagents_embedded = subagents_embedded.saturating_add(subagent_count);
        steps_exported = steps_exported.saturating_add(step_count);

        files.push(ArchiveAtifExportedFile {
            path: dest_path,
            filename,
            ctx_session_id: root.session_id.as_uuid().to_string(),
            provider: root.provider,
            provider_session_id: root.provider_session_id,
            steps: step_count,
            subagents: subagent_count,
        });
    }

    Ok(ArchiveAtifReceipt {
        schema_version: 1,
        operation: "archive_atif",
        atif_schema_version: ctx_history_capture::atif::SchemaVersion::CURRENT.as_str(),
        generation_id,
        output_dir: request.out_dir.clone(),
        sessions_exported: files.len(),
        subagents_embedded,
        steps_exported,
        files,
    })
}

fn is_embeddable_subagent(session: &SessionRecord) -> bool {
    match session.session_relationship {
        Some(
            ProviderNativeSessionRelationship::Delegated
            | ProviderNativeSessionRelationship::WorkflowChild,
        ) => true,
        Some(
            ProviderNativeSessionRelationship::Root
            | ProviderNativeSessionRelationship::ResumedFrom
            | ProviderNativeSessionRelationship::Forked,
        ) => false,
        None => session.agent_scope == Some(CoreAgentScope::Subagent),
    }
}

fn has_selected_embedding_ancestor(
    index: &VerifiedIndex,
    session: &SessionRecord,
    selected: &BTreeMap<Uuid, SessionRecord>,
) -> Result<bool> {
    if !is_embeddable_subagent(session) {
        return Ok(false);
    }
    let mut seen = HashSet::new();
    seen.insert(session.session_id.as_uuid());
    let mut current_parent = session.parent_session_id.map(|id| id.as_uuid());
    let mut hops = 0_usize;
    while let Some(parent_id) = current_parent {
        if hops >= MAX_SUBAGENT_NESTING_DEPTH || !seen.insert(parent_id) {
            break;
        }
        if selected.contains_key(&parent_id) {
            return Ok(true);
        }
        let Some(parent_record) = index.session_by_id(parent_id)? else {
            break;
        };
        if !is_embeddable_subagent(&parent_record) {
            break;
        }
        current_parent = parent_record.parent_session_id.map(|id| id.as_uuid());
        hops += 1;
    }
    Ok(false)
}

fn collect_matching_session_ids(
    index: &VerifiedIndex,
    selection: &CoreEventRangeSelection,
) -> Result<BTreeSet<Uuid>> {
    let mut ids = BTreeSet::new();
    let mut cursor = None;
    loop {
        let page = index.core_event_range_page_with_budget(
            selection,
            cursor.as_ref(),
            EXPORT_PAGE_SIZE,
            DEFAULT_CORE_EVENT_PAGE_BUDGET,
        )?;
        for event in page.items {
            ids.insert(event.core_record.session_id.as_uuid());
        }
        if page.terminal {
            break;
        }
        cursor =
            Some(page.next_cursor.ok_or_else(|| {
                anyhow!("incomplete event selection: missing continuation cursor")
            })?);
    }
    Ok(ids)
}

fn load_export_session_tree(
    index: &VerifiedIndex,
    session: &SessionRecord,
    max_events: Option<usize>,
    include_subagents: bool,
    depth: usize,
    visited: &mut HashSet<Uuid>,
) -> Result<AtifExportSession> {
    let session_uuid = session.session_id.as_uuid();
    visited.insert(session_uuid);

    let records = load_session_core_records(index, session_uuid, max_events)?;
    let mut children = Vec::new();
    if include_subagents && depth < MAX_SUBAGENT_NESTING_DEPTH {
        let child_ids = collect_direct_child_session_ids(index, session_uuid)?;
        let mut child_records = Vec::new();
        for child_id in child_ids {
            if visited.contains(&child_id) {
                continue;
            }
            if let Some(child_session) = index.session_by_id(child_id)? {
                if is_embeddable_subagent(&child_session) {
                    child_records.push(child_session);
                }
            }
        }
        child_records.sort_by(|left, right| {
            left.first_occurred_at_unix_ms
                .cmp(&right.first_occurred_at_unix_ms)
                .then_with(|| left.session_id.as_uuid().cmp(&right.session_id.as_uuid()))
        });
        for child_session in child_records {
            if visited.contains(&child_session.session_id.as_uuid()) {
                continue;
            }
            let child_tree =
                load_export_session_tree(index, &child_session, None, true, depth + 1, visited)?;
            if !child_tree.records.is_empty() {
                children.push(child_tree);
            }
        }
    }

    Ok(AtifExportSession {
        ctx_session_id: session_uuid,
        provider: parse_capture_provider(&session.provider),
        provider_session_id: session.provider_session_id.clone(),
        source_format: session.source_format.clone(),
        records,
        children,
    })
}

fn load_session_core_records(
    index: &VerifiedIndex,
    session_id: Uuid,
    max_events: Option<usize>,
) -> Result<Vec<CoreRecord>> {
    let selection = CoreEventRangeSelection::all(CoreEventRangeFilters {
        session_id: Some(session_id),
        direction: CoreEventRangeDirection::Ascending,
        ..CoreEventRangeFilters::default()
    })?;
    let mut records = Vec::new();
    let mut cursor = None;
    loop {
        let page = index.core_event_range_page_with_budget(
            &selection,
            cursor.as_ref(),
            EXPORT_PAGE_SIZE,
            DEFAULT_CORE_EVENT_PAGE_BUDGET,
        )?;
        for event in page.items {
            records.push(event.core_record);
            if let Some(limit) = max_events {
                if records.len() >= limit {
                    return Ok(records);
                }
            }
        }
        if page.terminal {
            break;
        }
        cursor = Some(page.next_cursor.ok_or_else(|| {
            anyhow!("incomplete session event scan: missing continuation cursor")
        })?);
    }
    Ok(records)
}

fn collect_direct_child_session_ids(
    index: &VerifiedIndex,
    parent_session_id: Uuid,
) -> Result<BTreeSet<Uuid>> {
    let selection = CoreEventRangeSelection::all(CoreEventRangeFilters {
        parent_session_id: Some(parent_session_id),
        direction: CoreEventRangeDirection::Ascending,
        ..CoreEventRangeFilters::default()
    })?;
    let mut ids = BTreeSet::new();
    let mut cursor = None;
    loop {
        let page = index.core_event_range_page_with_budget(
            &selection,
            cursor.as_ref(),
            EXPORT_PAGE_SIZE,
            DEFAULT_CORE_EVENT_PAGE_BUDGET,
        )?;
        for event in page.items {
            let child_id = event.core_record.session_id.as_uuid();
            if child_id != parent_session_id {
                ids.insert(child_id);
            }
        }
        if page.terminal {
            break;
        }
        cursor = Some(page.next_cursor.ok_or_else(|| {
            anyhow!("incomplete child session scan: missing continuation cursor")
        })?);
    }
    Ok(ids)
}

fn parse_capture_provider(raw: &str) -> CaptureProvider {
    raw.parse::<CaptureProvider>()
        .unwrap_or(CaptureProvider::Custom)
}

fn count_session_records(session: &AtifExportSession) -> usize {
    session.records.len()
        + session
            .children
            .iter()
            .map(count_session_records)
            .sum::<usize>()
}

fn count_embedded_subagents(trajectory: &Trajectory) -> usize {
    trajectory
        .subagent_trajectories
        .as_deref()
        .map(|subagents| {
            subagents.len()
                + subagents
                    .iter()
                    .map(count_embedded_subagents)
                    .sum::<usize>()
        })
        .unwrap_or(0)
}

fn write_file_atomically(dest: &Path, bytes: &[u8]) -> Result<()> {
    let parent = dest.parent().ok_or_else(|| {
        anyhow!(
            "ATIF export target has no parent directory: {}",
            dest.display()
        )
    })?;
    let temp_name = format!(".ctx-atif-{}.tmp", Uuid::new_v4());
    let temp_path = parent.join(temp_name);
    let write_result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .with_context(|| format!("create temporary ATIF file {}", temp_path.display()))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }
    if let Err(error) = fs::rename(&temp_path, dest) {
        let _ = fs::remove_file(&temp_path);
        return Err(error).with_context(|| format!("publish ATIF file {}", dest.display()));
    }
    Ok(())
}
