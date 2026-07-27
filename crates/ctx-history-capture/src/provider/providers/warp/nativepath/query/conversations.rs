use super::*;

pub(super) fn scan_conversations(
    conn: &Connection,
    counters: &mut WarpNativeCounters,
    resume: &WarpNativeFrontier,
) -> Result<(
    BTreeMap<String, WarpHierarchyNode>,
    Vec<WarpConversationEmission>,
)> {
    let _guard = SqliteLengthPreflightGuard::new(conn);
    let mut statement = prepare_conversation_candidates(conn)?;
    let limit = conversation_hydration_limit()?;
    let after_rowid = if resume.phase == WarpNativeFrontierPhase::Conversations {
        resume.last_conversation_rowid.ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Warp conversation resume frontier omitted its rowid".to_owned(),
            )
        })?
    } else {
        0
    };
    let mut rows = statement.query(rusqlite::params![limit, after_rowid])?;
    let mut hierarchy = BTreeMap::new();
    let mut emissions = Vec::new();
    while let Some(row) = rows.next()? {
        counters.conversation_rows = counters.conversation_rows.saturating_add(1);
        let candidate = conversation_candidate_from_row(row)?;
        if let Some(rejection) = reject_conversation_candidate(&candidate)? {
            emissions.push(WarpConversationEmission::Rejection {
                rejection,
                rowid: candidate.rowid,
                source_digest: rejected_conversation_candidate_digest(&candidate)?,
            });
            continue;
        }
        let (Some(conversation_id), Some(raw_data), Some(raw_modified)) = (
            candidate.hydrated_conversation_id,
            candidate.hydrated_conversation_data,
            candidate.hydrated_last_modified_at,
        ) else {
            return Err(CaptureError::SystemInvariant(
                "Warp conversation passed preflight without bounded hydrated values",
            ));
        };
        counters.conversation_rows_hydrated = counters.conversation_rows_hydrated.saturating_add(1);
        let evidence_digest = source_text_row_digest(
            b"conversation\0",
            [&conversation_id, &raw_data, &raw_modified],
        )?;
        counters.conversation_json_objects_parsed =
            counters.conversation_json_objects_parsed.saturating_add(1);
        let mut rejections = Vec::new();
        let conversation_data =
            parse_conversation_data(&raw_data, &conversation_id, &mut rejections);
        let modified_at =
            parse_optional_conversation_timestamp(&raw_modified, &conversation_id, &mut rejections);
        let parent_conversation_id = conversation_data
            .get("parent_conversation_id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .filter(|value| !value.is_empty());
        let title = conversation_data
            .get("agent_name")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(|value| truncate_chars(value, 240))
            .unwrap_or_else(|| format!("Warp {conversation_id}"));
        let metadata = bounded_session_metadata(&conversation_data)?;
        counters.peak_session_metadata_rows = counters.peak_session_metadata_rows.max(1);
        let node = WarpHierarchyNode {
            parent_conversation_id,
            root_conversation_id: conversation_id.clone(),
            root_resolved: false,
            parent_present: false,
            title,
            modified_at,
            metadata,
            rejections,
        };
        if hierarchy.insert(conversation_id.clone(), node).is_some() {
            return Err(CaptureError::InvalidPayload(format!(
                "Warp source contains duplicate conversation identity {conversation_id:?}"
            )));
        }
        emissions.push(WarpConversationEmission::Session {
            conversation_id,
            rowid: candidate.rowid,
            source_digest: evidence_digest,
        });
    }
    let pending = hierarchy
        .values()
        .filter_map(|node| node.parent_conversation_id.clone())
        .filter(|parent| !hierarchy.contains_key(parent))
        .collect::<Vec<_>>();
    load_hierarchy_closure(conn, pending, &mut hierarchy, counters)?;
    resolve_hierarchy(&mut hierarchy)?;
    Ok((hierarchy, emissions))
}

pub(super) fn load_task_hierarchy(
    conn: &Connection,
    schema: &WarpSqliteSchema,
    resume: &WarpNativeFrontier,
    mut hierarchy: BTreeMap<String, WarpHierarchyNode>,
    counters: &mut WarpNativeCounters,
) -> Result<BTreeMap<String, WarpHierarchyNode>> {
    let task_resume = resume.phase == WarpNativeFrontierPhase::Tasks;
    let comparison = if !task_resume {
        "1 = 1"
    } else if resume.next_message_ordinal == 0 {
        "t.task_id collate binary > (
             select previous.task_id from agent_tasks previous
             where previous.rowid = ?1
         )"
    } else {
        "t.rowid = ?1 or t.task_id collate binary > (
             select previous.task_id from agent_tasks previous
             where previous.rowid = ?1
         )"
    };
    let index = warp_quote_identifier(&schema.task_keyset_index);
    let mut task_conversations = conn.prepare(&format!(
        "select distinct t.conversation_id
         from agent_tasks t indexed by {index}
         where typeof(t.conversation_id) = 'text'
           and ({comparison})
         order by t.conversation_id collate binary"
    ))?;
    let mut pending = Vec::new();
    {
        let _guard = SqliteLengthPreflightGuard::new(conn);
        let mut rows = if task_resume {
            let last_task_rowid = resume.last_task_rowid.ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "Warp task resume frontier omitted its rowid".to_owned(),
                )
            })?;
            task_conversations.query([last_task_rowid])?
        } else {
            task_conversations.query([])?
        };
        while let Some(row) = rows.next()? {
            pending.push(row.get::<_, String>(0)?);
        }
    }
    load_hierarchy_closure(conn, pending, &mut hierarchy, counters)?;
    resolve_hierarchy(&mut hierarchy)?;
    counters.hierarchy_nodes_retained = u64::try_from(hierarchy.len()).unwrap_or(u64::MAX);
    counters.hierarchy_edges = hierarchy
        .values()
        .filter(|node| node.parent_conversation_id.is_some())
        .count()
        .try_into()
        .unwrap_or(u64::MAX);
    Ok(hierarchy)
}

fn load_hierarchy_closure(
    conn: &Connection,
    mut pending: Vec<String>,
    hierarchy: &mut BTreeMap<String, WarpHierarchyNode>,
    counters: &mut WarpNativeCounters,
) -> Result<()> {
    let limit = conversation_hydration_limit()?;
    let mut conversation = conn.prepare(
        "select rowid, \
                typeof(conversation_id), coalesce(octet_length(conversation_id), 0), \
                typeof(conversation_data), coalesce(octet_length(conversation_data), 0), \
                typeof(last_modified_at), coalesce(octet_length(last_modified_at), 0), \
                case when typeof(conversation_id) = 'text' \
                           and typeof(conversation_data) = 'text' \
                           and typeof(last_modified_at) = 'text' \
                           and coalesce(octet_length(conversation_id), 0) \
                             + coalesce(octet_length(conversation_data), 0) \
                             + coalesce(octet_length(last_modified_at), 0) <= ?2 \
                     then conversation_id end, \
                case when typeof(conversation_id) = 'text' \
                           and typeof(conversation_data) = 'text' \
                           and typeof(last_modified_at) = 'text' \
                           and coalesce(octet_length(conversation_id), 0) \
                             + coalesce(octet_length(conversation_data), 0) \
                             + coalesce(octet_length(last_modified_at), 0) <= ?2 \
                     then conversation_data end, \
                case when typeof(conversation_id) = 'text' \
                           and typeof(conversation_data) = 'text' \
                           and typeof(last_modified_at) = 'text' \
                           and coalesce(octet_length(conversation_id), 0) \
                             + coalesce(octet_length(conversation_data), 0) \
                             + coalesce(octet_length(last_modified_at), 0) <= ?2 \
                     then last_modified_at end \
         from agent_conversations
         where conversation_id = ?1 collate binary
         limit 1",
    )?;
    let mut seen = hierarchy.keys().cloned().collect::<BTreeSet<_>>();
    while let Some(requested_id) = pending.pop() {
        if !seen.insert(requested_id.clone()) {
            continue;
        }
        let _guard = SqliteLengthPreflightGuard::new(conn);
        let candidate = conversation
            .query_row(
                rusqlite::params![requested_id, limit],
                conversation_candidate_from_row,
            )
            .optional()?;
        let Some(candidate) = candidate else {
            continue;
        };
        counters.conversation_rows = counters.conversation_rows.saturating_add(1);
        if reject_conversation_candidate(&candidate)?.is_some() {
            continue;
        }
        let (Some(conversation_id), Some(raw_data), Some(raw_modified)) = (
            candidate.hydrated_conversation_id,
            candidate.hydrated_conversation_data,
            candidate.hydrated_last_modified_at,
        ) else {
            return Err(CaptureError::SystemInvariant(
                "Warp resume conversation passed preflight without bounded values",
            ));
        };
        counters.conversation_rows_hydrated = counters.conversation_rows_hydrated.saturating_add(1);
        counters.conversation_json_objects_parsed =
            counters.conversation_json_objects_parsed.saturating_add(1);
        let mut rejections = Vec::new();
        let conversation_data =
            parse_conversation_data(&raw_data, &conversation_id, &mut rejections);
        let modified_at =
            parse_optional_conversation_timestamp(&raw_modified, &conversation_id, &mut rejections);
        let parent_conversation_id = conversation_data
            .get("parent_conversation_id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .filter(|value| !value.is_empty());
        if let Some(parent) = parent_conversation_id.as_ref() {
            pending.push(parent.clone());
        }
        let title = conversation_data
            .get("agent_name")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(|value| truncate_chars(value, 240))
            .unwrap_or_else(|| format!("Warp {conversation_id}"));
        let metadata = bounded_session_metadata(&conversation_data)?;
        counters.peak_session_metadata_rows = counters.peak_session_metadata_rows.max(1);
        hierarchy.insert(
            conversation_id.clone(),
            WarpHierarchyNode {
                parent_conversation_id,
                root_conversation_id: conversation_id,
                root_resolved: false,
                parent_present: false,
                title,
                modified_at,
                metadata,
                rejections,
            },
        );
    }
    Ok(())
}

fn prepare_conversation_candidates(conn: &Connection) -> Result<Statement<'_>> {
    conn.prepare(
        "select rowid, \
                typeof(conversation_id), coalesce(octet_length(conversation_id), 0), \
                typeof(conversation_data), coalesce(octet_length(conversation_data), 0), \
                typeof(last_modified_at), coalesce(octet_length(last_modified_at), 0), \
                case when typeof(conversation_id) = 'text' \
                           and typeof(conversation_data) = 'text' \
                           and typeof(last_modified_at) = 'text' \
                           and coalesce(octet_length(conversation_id), 0) \
                             + coalesce(octet_length(conversation_data), 0) \
                             + coalesce(octet_length(last_modified_at), 0) <= ?1 \
                     then conversation_id end, \
                case when typeof(conversation_id) = 'text' \
                           and typeof(conversation_data) = 'text' \
                           and typeof(last_modified_at) = 'text' \
                           and coalesce(octet_length(conversation_id), 0) \
                             + coalesce(octet_length(conversation_data), 0) \
                             + coalesce(octet_length(last_modified_at), 0) <= ?1 \
                     then conversation_data end, \
                case when typeof(conversation_id) = 'text' \
                           and typeof(conversation_data) = 'text' \
                           and typeof(last_modified_at) = 'text' \
                           and coalesce(octet_length(conversation_id), 0) \
                             + coalesce(octet_length(conversation_data), 0) \
                             + coalesce(octet_length(last_modified_at), 0) <= ?1 \
                     then last_modified_at end \
         from agent_conversations \
         where rowid > ?2 \
         order by rowid",
    )
    .map_err(CaptureError::from)
}

fn conversation_hydration_limit() -> Result<i64> {
    let maximum = u64::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).map_err(|_| {
        CaptureError::SystemInvariant("Warp NativePath SQLite byte limit exceeds u64")
    })?;
    let payload = maximum
        .checked_sub(WARP_NATIVE_SQLITE_CONVERSATION_ROW_OVERHEAD_BYTES)
        .ok_or(CaptureError::SystemInvariant(
            "Warp NativePath conversation row overhead exceeds its byte limit",
        ))?;
    i64::try_from(payload).map_err(|_| {
        CaptureError::SystemInvariant("Warp NativePath conversation byte limit exceeds i64")
    })
}

fn conversation_candidate_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<WarpConversationCandidate> {
    Ok(WarpConversationCandidate {
        rowid: row.get(0)?,
        conversation_id: WarpTaskCellMetadata {
            storage_class: row.get(1)?,
            bytes: row.get(2)?,
        },
        conversation_data: WarpTaskCellMetadata {
            storage_class: row.get(3)?,
            bytes: row.get(4)?,
        },
        last_modified_at: WarpTaskCellMetadata {
            storage_class: row.get(5)?,
            bytes: row.get(6)?,
        },
        hydrated_conversation_id: row.get(7)?,
        hydrated_conversation_data: row.get(8)?,
        hydrated_last_modified_at: row.get(9)?,
    })
}

pub(super) fn emit_sessions_and_hierarchy(
    hierarchy: &BTreeMap<String, WarpHierarchyNode>,
    emissions: Vec<WarpConversationEmission>,
    builder: &mut WarpNativePageEmitter<'_>,
    counters: &mut WarpNativeCounters,
    resume: &WarpNativeFrontier,
) -> Result<()> {
    let mut completed_conversations = resume.completed_conversation_rows;
    let mut completed_edges = resume.completed_hierarchy_edges;
    for emission in emissions {
        let mut unit = WarpNativeUnit::progress();
        let (native_key, rowid, source_digest) = match emission {
            WarpConversationEmission::Session {
                conversation_id,
                rowid,
                source_digest,
            } => {
                let node = hierarchy.get(&conversation_id).ok_or_else(|| {
                    CaptureError::InvalidPayload(format!(
                        "Warp hierarchy index omitted conversation {conversation_id:?}"
                    ))
                })?;
                for rejection in &node.rejections {
                    unit.push_rejection(rejection.clone())?;
                }
                unit.push_session(WarpNativeSession {
                    conversation_id: conversation_id.clone(),
                    parent_conversation_id: node.parent_conversation_id.clone(),
                    root_conversation_id: node.root_conversation_id.clone(),
                    parent_present: node.parent_present,
                    title: node.title.clone(),
                    modified_at: node.modified_at,
                    metadata: node.metadata.clone(),
                })?;
                if let Some(parent) = node.parent_conversation_id.as_ref() {
                    unit.push_edge(WarpNativeHierarchyEdge {
                        child_conversation_id: conversation_id.clone(),
                        parent_conversation_id: parent.clone(),
                        parent_present: node.parent_present,
                    })?;
                    completed_edges = completed_edges.saturating_add(1);
                }
                counters.sessions_retained = counters.sessions_retained.saturating_add(1);
                (conversation_id, rowid, source_digest)
            }
            WarpConversationEmission::Rejection {
                rejection,
                rowid,
                source_digest,
            } => {
                let native_key = rejection.native_key.clone();
                unit.push_rejection(rejection)?;
                (native_key, rowid, source_digest)
            }
        };
        builder.record_source(b"conversation\0", source_digest)?;
        completed_conversations = completed_conversations.saturating_add(1);
        builder.push(
            unit,
            WarpNativeFrontier::after_conversation(completed_conversations, completed_edges, rowid),
            native_key,
            counters,
        )?;
    }
    Ok(())
}

fn reject_conversation_candidate(
    candidate: &WarpConversationCandidate,
) -> Result<Option<WarpNativeRejection>> {
    let native_key = format!("rowid:{}", candidate.rowid);
    for (field, metadata) in [
        ("conversation_id", &candidate.conversation_id),
        ("conversation_data", &candidate.conversation_data),
        ("last_modified_at", &candidate.last_modified_at),
    ] {
        if metadata.storage_class != "text" {
            return Ok(Some(WarpNativeRejection {
                kind: WarpNativeRejectionKind::ConversationRecord,
                native_key,
                reason: format!(
                    "Warp conversation {field} must use SQLite TEXT storage \
                     (observed {})",
                    metadata.storage_class
                ),
            }));
        }
    }
    let observed_bytes = [
        ("conversation_id", &candidate.conversation_id),
        ("conversation_data", &candidate.conversation_data),
        ("last_modified_at", &candidate.last_modified_at),
    ]
    .into_iter()
    .try_fold(
        WARP_NATIVE_SQLITE_CONVERSATION_ROW_OVERHEAD_BYTES,
        |total, (field, metadata)| {
            total
                .checked_add(metadata.observed_bytes(field)?)
                .ok_or(CaptureError::SystemInvariant(
                    "Warp NativePath conversation row byte count overflowed",
                ))
        },
    )?;
    let limit = u64::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).map_err(|_| {
        CaptureError::SystemInvariant("Warp NativePath SQLite byte limit exceeds u64")
    })?;
    if observed_bytes > limit {
        return Ok(Some(WarpNativeRejection {
            kind: WarpNativeRejectionKind::ConversationRecord,
            native_key,
            reason: format!(
                "Warp conversation row exceeds \
                 {MAX_PROVIDER_SQLITE_VALUE_BYTES}-byte hydration limit \
                 ({observed_bytes} bytes)"
            ),
        }));
    }
    if candidate
        .hydrated_conversation_id
        .as_deref()
        .is_none_or(str::is_empty)
    {
        return Ok(Some(WarpNativeRejection {
            kind: WarpNativeRejectionKind::ConversationRecord,
            native_key,
            reason: "Warp conversation_id is empty".to_owned(),
        }));
    }
    if candidate.hydrated_conversation_data.is_none()
        || candidate.hydrated_last_modified_at.is_none()
    {
        return Err(CaptureError::SystemInvariant(
            "Warp conversation metadata preflight omitted a bounded value",
        ));
    }
    Ok(None)
}

fn parse_conversation_data(
    raw_data: &str,
    conversation_id: &str,
    rejections: &mut Vec<WarpNativeRejection>,
) -> Value {
    match serde_json::from_str::<Value>(raw_data) {
        Ok(Value::Object(value)) => Value::Object(value),
        Ok(_) => {
            rejections.push(WarpNativeRejection {
                kind: WarpNativeRejectionKind::ConversationRecord,
                native_key: conversation_id.to_owned(),
                reason: "Warp conversation_data must be a JSON object".to_owned(),
            });
            Value::Object(serde_json::Map::new())
        }
        Err(error) => {
            rejections.push(WarpNativeRejection {
                kind: WarpNativeRejectionKind::ConversationRecord,
                native_key: conversation_id.to_owned(),
                reason: format!("invalid Warp conversation_data JSON: {error}"),
            });
            Value::Object(serde_json::Map::new())
        }
    }
}

fn parse_optional_conversation_timestamp(
    raw_modified: &str,
    conversation_id: &str,
    rejections: &mut Vec<WarpNativeRejection>,
) -> Option<DateTime<Utc>> {
    if raw_modified.is_empty() {
        return None;
    }
    match parse_warp_timestamp(raw_modified) {
        Ok(value) => Some(value),
        Err(error) => {
            rejections.push(WarpNativeRejection {
                kind: WarpNativeRejectionKind::ConversationRecord,
                native_key: conversation_id.to_owned(),
                reason: error.to_string(),
            });
            None
        }
    }
}

fn resolve_hierarchy(hierarchy: &mut BTreeMap<String, WarpHierarchyNode>) -> Result<()> {
    let conversation_ids = hierarchy.keys().cloned().collect::<Vec<_>>();
    for conversation_id in &conversation_ids {
        resolve_hierarchy_root(conversation_id, hierarchy)?;
    }
    for conversation_id in conversation_ids {
        let parent_present = hierarchy
            .get(&conversation_id)
            .and_then(|node| node.parent_conversation_id.as_ref())
            .is_some_and(|parent| hierarchy.contains_key(parent));
        let node = hierarchy
            .get_mut(&conversation_id)
            .ok_or(CaptureError::SystemInvariant(
                "Warp hierarchy node disappeared during resolution",
            ))?;
        node.parent_present = parent_present;
    }
    Ok(())
}

fn resolve_hierarchy_root(
    conversation_id: &str,
    hierarchy: &mut BTreeMap<String, WarpHierarchyNode>,
) -> Result<()> {
    let mut seen = BTreeSet::new();
    let mut path = Vec::new();
    let mut current = conversation_id.to_owned();
    let root_conversation_id = loop {
        if !seen.insert(current.clone()) {
            return Err(CaptureError::InvalidPayload(format!(
                "Warp conversation hierarchy contains a cycle at {current:?}"
            )));
        }
        let Some(node) = hierarchy.get(&current) else {
            break current;
        };
        if node.root_resolved {
            break node.root_conversation_id.clone();
        }
        path.push(current.clone());
        let Some(parent) = node.parent_conversation_id.as_deref() else {
            break current;
        };
        current = parent.to_owned();
    };
    for conversation_id in path {
        let node = hierarchy
            .get_mut(&conversation_id)
            .ok_or(CaptureError::SystemInvariant(
                "Warp hierarchy node disappeared during root caching",
            ))?;
        node.root_conversation_id.clone_from(&root_conversation_id);
        node.root_resolved = true;
    }
    Ok(())
}
