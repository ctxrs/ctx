use std::collections::BTreeSet;

use rusqlite::Connection;
use uuid::Uuid;

use super::{JournalEvidenceIdentity, JournalProvenanceIdentity};
use crate::{CanonicalObservation, Result, StoreError};

pub(super) const MAX_AUTHORIZED_REPOSITORY_ROOTS: usize = 128;
const MAX_AUTHORIZED_REPOSITORY_ROOT_BYTES: usize = 4 * 1024;
const MAX_AUTHORIZED_REPOSITORY_ROOTS_TOTAL_BYTES: usize = 256 * 1024;

pub(super) fn authorized_repository_roots(conn: &Connection) -> Result<Vec<String>> {
    let mut statement = conn.prepare(
        "SELECT candidate FROM (
             SELECT 0 AS priority, display_root AS candidate FROM local_workspaces
             UNION ALL
             SELECT 1, root_path FROM vcs_workspaces WHERE deleted_at_ms IS NULL
             UNION ALL
             SELECT 2, r.cwd FROM runs r
              WHERE r.deleted_at_ms IS NULL AND r.cwd IS NOT NULL
                AND EXISTS (SELECT 1 FROM events e WHERE e.run_id = r.id AND e.deleted_at_ms IS NULL)
             UNION ALL
             SELECT 3, cs.cwd FROM capture_sources cs
              WHERE cs.cwd IS NOT NULL AND (
                EXISTS (SELECT 1 FROM events e WHERE e.capture_source_id = cs.id AND e.deleted_at_ms IS NULL)
                OR EXISTS (SELECT 1 FROM sessions s WHERE s.capture_source_id = cs.id AND s.deleted_at_ms IS NULL)
                OR EXISTS (SELECT 1 FROM runs r WHERE r.source_id = cs.id AND r.deleted_at_ms IS NULL)
              )
         ) WHERE candidate IS NOT NULL ORDER BY priority, candidate",
    )?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut seen = BTreeSet::new();
    let mut roots = Vec::new();
    let mut total_bytes = 0_usize;
    for row in rows {
        let root = row?;
        if !valid_identity(&root) || !seen.insert(root.clone()) {
            continue;
        }
        let Some(next_total) = total_bytes.checked_add(root.len()) else {
            break;
        };
        if roots.len() == MAX_AUTHORIZED_REPOSITORY_ROOTS
            || next_total > MAX_AUTHORIZED_REPOSITORY_ROOTS_TOTAL_BYTES
        {
            continue;
        }
        total_bytes = next_total;
        roots.push(root);
    }
    roots.sort();
    Ok(roots)
}

pub(super) fn sanitize_canonical_observation(observation: &mut CanonicalObservation) -> Result<()> {
    required_uuid(observation.observation_id, "observation ID")?;
    optional_uuid(observation.event_id, "event ID")?;
    optional_uuid(observation.history_record_id, "history record ID")?;
    if let Some(actor) = &mut observation.actor {
        required_uuid(actor.direct_session_id, "direct session ID")?;
        required_uuid(actor.root_session_id, "root session ID")?;
        optional_uuid(actor.parent_session_id, "parent session ID")?;
        required_identity(&actor.agent_type, "agent type")?;
        sanitize_optional_identity(&mut actor.external_session_id);
        sanitize_optional_identity(&mut actor.external_agent_id);
        sanitize_optional_identity(&mut actor.role_hint);
    }
    if let Some(run) = &mut observation.run {
        required_uuid(run.id, "run ID")?;
        required_identity(&run.run_type, "run type")?;
        required_identity(&run.status, "run status")?;
    }
    if let Some(source) = &mut observation.source {
        required_uuid(source.id, "source ID")?;
        required_identity(&source.provider, "source provider")?;
        sanitize_optional_identity(&mut source.path);
        sanitize_optional_identity(&mut source.format);
        sanitize_optional_identity(&mut source.identity);
    }
    required_uuid(
        observation.citation.observation_id,
        "citation observation ID",
    )?;
    optional_uuid(observation.citation.event_id, "citation event ID")?;
    sanitize_optional_identity(&mut observation.citation.source_path);
    if observation.citation.source_record_ordinal.is_none() {
        observation.citation.source_record_subrecord_index = None;
    }
    if let Some(touch) = &mut observation.file_touch {
        required_uuid(touch.id, "file touch ID")?;
        optional_uuid(touch.history_record_id, "file history record ID")?;
        optional_uuid(touch.run_id, "file run ID")?;
        optional_uuid(touch.event_id, "file event ID")?;
        optional_uuid(touch.vcs_workspace_id, "file VCS workspace ID")?;
        optional_uuid(touch.source_id, "file source ID")?;
        required_identity(&touch.path, "file path")?;
        sanitize_optional_identity(&mut touch.old_path);
    }
    if let Some(change) = &mut observation.vcs_change {
        required_uuid(change.id, "VCS change ID")?;
        required_uuid(change.vcs_workspace_id, "VCS workspace ID")?;
        optional_uuid(change.source_id, "VCS source ID")?;
        required_identity(&change.change_id, "VCS change identifier")?;
        change
            .parent_change_ids
            .retain(|value| valid_identity(value));
        sanitize_optional_identity(&mut change.branch_or_bookmark);
        sanitize_optional_identity(&mut change.tree_hash);
    }
    Ok(())
}

pub(super) fn validate_stored_evidence(evidence: &[JournalEvidenceIdentity]) -> Result<()> {
    for identity in evidence {
        required_uuid(identity.event_id, "evidence event ID")?;
        optional_uuid(identity.source_id, "evidence source ID")?;
        if identity
            .source_path
            .as_deref()
            .is_some_and(|path| !valid_identity(path))
        {
            return Err(StoreError::InvalidProjectionJournalData(
                "evidence source path is empty, unsafe, or overbound".to_owned(),
            ));
        }
        if identity.source_record_subrecord_index.is_some()
            && identity.source_record_ordinal.is_none()
        {
            return Err(StoreError::InvalidProjectionJournalData(
                "evidence subrecord requires a record ordinal".to_owned(),
            ));
        }
        match (identity.byte_start, identity.byte_end_exclusive) {
            (None, None) => {}
            (Some(start), Some(end)) if start <= end => {}
            _ => {
                return Err(StoreError::InvalidProjectionJournalData(
                    "evidence byte range is incomplete or invalid".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

pub(super) fn validate_stored_provenance(provenance: &JournalProvenanceIdentity) -> Result<()> {
    required_uuid(provenance.stable_entity_id, "provenance stable entity ID")?;
    optional_uuid(provenance.capture_source_id, "provenance capture source ID")?;
    if provenance
        .provider
        .as_deref()
        .is_some_and(|provider| !valid_identity(provider))
        || provenance
            .provider_external_id
            .as_deref()
            .is_some_and(|external_id| !valid_identity(external_id))
    {
        return Err(StoreError::InvalidProjectionJournalData(
            "provenance identity is empty, unsafe, or overbound".to_owned(),
        ));
    }
    Ok(())
}

pub(super) fn parse_uuid(value: &str) -> Result<Uuid> {
    let parsed = Uuid::parse_str(value).map_err(StoreError::from)?;
    required_uuid(parsed, "journal UUID")?;
    Ok(parsed)
}

fn required_uuid(value: Uuid, field: &str) -> Result<()> {
    if value.is_nil() {
        return Err(StoreError::InvalidProjectionJournalData(format!(
            "nil {field} is not Protocol V1-valid"
        )));
    }
    Ok(())
}

fn optional_uuid(value: Option<Uuid>, field: &str) -> Result<()> {
    if value.is_some_and(|value| value.is_nil()) {
        return Err(StoreError::InvalidProjectionJournalData(format!(
            "nil {field} is not Protocol V1-valid"
        )));
    }
    Ok(())
}

fn valid_identity(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= MAX_AUTHORIZED_REPOSITORY_ROOT_BYTES
        && !value.chars().any(char::is_control)
}

fn required_identity(value: &str, field: &str) -> Result<()> {
    if !valid_identity(value) {
        return Err(StoreError::InvalidProjectionJournalData(format!(
            "{field} is empty, unsafe, or exceeds the Protocol V1 identity bound"
        )));
    }
    Ok(())
}

fn sanitize_optional_identity(value: &mut Option<String>) {
    if value.as_deref().is_some_and(|value| !valid_identity(value)) {
        *value = None;
    }
}
