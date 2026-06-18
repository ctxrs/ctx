use super::*;

impl Store {
    pub async fn upsert_change_set(&self, change_set: &ChangeSet) -> Result<ChangeSet> {
        let mut record = change_set.clone();
        validate_agent_work_schema_version(record.schema_version, "change set")?;
        validate_agent_work_source_records(&record.source_records, "change set")?;
        self.validate_change_set_workspace(&record).await?;
        let now = Utc::now();
        let created_at = match record.created_at {
            Some(created_at) => created_at,
            None => self
                .change_set_created_at(record.workspace_id, record.id.clone())
                .await?
                .unwrap_or(now),
        };
        let updated_at = record.updated_at.unwrap_or(now);
        record.created_at = Some(created_at);
        record.updated_at = Some(updated_at);
        let record_json =
            serde_json::to_string(&record).context("serializing change set record")?;

        let result = self
            .query(
                r#"INSERT INTO change_sets (
                    id, workspace_id, source_worktree_id, base_revision, head_revision,
                    target_branch, record_json, created_at, updated_at
               )
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
               ON CONFLICT(id) DO UPDATE SET
                    source_worktree_id = excluded.source_worktree_id,
                    base_revision = excluded.base_revision,
                    head_revision = excluded.head_revision,
                    target_branch = excluded.target_branch,
                    record_json = excluded.record_json,
                    updated_at = excluded.updated_at
                 WHERE change_sets.workspace_id = excluded.workspace_id"#,
            )
            .bind(record.id.0.to_string())
            .bind(record.workspace_id.0.to_string())
            .bind(record.source_worktree_id.map(|id| id.0.to_string()))
            .bind(record.base_revision.as_deref())
            .bind(record.head_revision.as_deref())
            .bind(record.target_branch.as_deref())
            .bind(record_json)
            .bind(created_at.to_rfc3339())
            .bind(updated_at.to_rfc3339())
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            anyhow::bail!("change set id already exists in a different workspace");
        }

        Ok(record)
    }

    pub async fn get_change_set(&self, id: ChangeSetId) -> Result<Option<ChangeSet>> {
        let row = self
            .query(r#"SELECT record_json, source_worktree_id FROM change_sets WHERE id = ?"#)
            .bind(id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;

        row.map(decode_change_set_row).transpose()
    }

    pub async fn get_workspace_change_set(
        &self,
        workspace_id: WorkspaceId,
        id: ChangeSetId,
    ) -> Result<Option<ChangeSet>> {
        let row = self
            .query(
                r#"SELECT record_json, source_worktree_id
                   FROM change_sets
                   WHERE id = ? AND workspace_id = ?"#,
            )
            .bind(id.0.to_string())
            .bind(workspace_id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;

        row.map(decode_change_set_row).transpose()
    }

    pub async fn list_workspace_change_sets(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Vec<ChangeSet>> {
        let rows = self
            .query(
                r#"SELECT record_json, source_worktree_id
                   FROM change_sets
                   WHERE workspace_id = ?
                   ORDER BY updated_at DESC, id DESC"#,
            )
            .bind(workspace_id.0.to_string())
            .fetch_all(&self.pool)
            .await?;

        rows.into_iter().map(decode_change_set_row).collect()
    }

    async fn change_set_created_at(
        &self,
        workspace_id: WorkspaceId,
        id: ChangeSetId,
    ) -> Result<Option<DateTime<Utc>>> {
        let raw = self
            .query_scalar::<String>(
                r#"SELECT created_at FROM change_sets WHERE id = ? AND workspace_id = ?"#,
            )
            .bind(id.0.to_string())
            .bind(workspace_id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;

        raw.as_deref().map(parse_dt).transpose()
    }

    pub async fn upsert_contribution(&self, contribution: &Contribution) -> Result<Contribution> {
        let mut record = contribution.clone();
        validate_agent_work_schema_version(record.schema_version, "contribution")?;
        validate_agent_work_source_records(&record.source_records, "contribution")?;
        self.validate_contribution_workspace(&record).await?;
        let now = Utc::now();
        let created_at = match record.created_at {
            Some(created_at) => created_at,
            None => self
                .contribution_created_at(record.workspace_id, record.id.clone())
                .await?
                .unwrap_or(now),
        };
        let updated_at = record.updated_at.unwrap_or(now);
        record.created_at = Some(created_at);
        record.updated_at = Some(updated_at);
        let (subject_kind, subject_id) = contribution_endpoint_index(&record.subject);
        let (target_kind, target_id) = contribution_endpoint_index(&record.target);
        let record_json =
            serde_json::to_string(&record).context("serializing contribution record")?;

        let result = self
            .query(
            r#"INSERT INTO contributions (
                    id, workspace_id, change_set_id, subject_kind, subject_id, target_kind, target_id,
                    record_json, created_at, updated_at
               )
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
               ON CONFLICT(id) DO UPDATE SET
                    change_set_id = excluded.change_set_id,
                    subject_kind = excluded.subject_kind,
                    subject_id = excluded.subject_id,
                    target_kind = excluded.target_kind,
                    target_id = excluded.target_id,
                    record_json = excluded.record_json,
                    updated_at = excluded.updated_at
                 WHERE contributions.workspace_id = excluded.workspace_id"#,
            )
        .bind(record.id.0.to_string())
        .bind(record.workspace_id.0.to_string())
        .bind(record.change_set_id.as_ref().map(|id| id.0.to_string()))
        .bind(subject_kind)
        .bind(subject_id)
        .bind(target_kind)
        .bind(target_id)
        .bind(record_json)
        .bind(created_at.to_rfc3339())
        .bind(updated_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            anyhow::bail!("contribution id already exists in a different workspace");
        }

        Ok(record)
    }

    pub async fn get_contribution(&self, id: ContributionId) -> Result<Option<Contribution>> {
        let row = self
            .query(r#"SELECT record_json, change_set_id FROM contributions WHERE id = ?"#)
            .bind(id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;

        row.map(decode_contribution_row).transpose()
    }

    pub async fn list_workspace_contributions(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Vec<Contribution>> {
        let rows = self
            .query(
                r#"SELECT record_json, change_set_id
                   FROM contributions
                   WHERE workspace_id = ?
                   ORDER BY updated_at DESC, id DESC"#,
            )
            .bind(workspace_id.0.to_string())
            .fetch_all(&self.pool)
            .await?;

        rows.into_iter().map(decode_contribution_row).collect()
    }

    pub async fn list_contributions_for_change_set(
        &self,
        workspace_id: WorkspaceId,
        change_set_id: ChangeSetId,
    ) -> Result<Vec<Contribution>> {
        let rows = self
            .query(
                r#"SELECT record_json, change_set_id
                   FROM contributions
                   WHERE workspace_id = ?
                     AND change_set_id = ?
                   ORDER BY updated_at DESC, id DESC"#,
            )
            .bind(workspace_id.0.to_string())
            .bind(change_set_id.0.to_string())
            .fetch_all(&self.pool)
            .await?;

        rows.into_iter().map(decode_contribution_row).collect()
    }

    pub async fn list_contributions_for_endpoint(
        &self,
        workspace_id: WorkspaceId,
        endpoint: &ContributionEndpoint,
    ) -> Result<Vec<Contribution>> {
        let (endpoint_kind, endpoint_id) = contribution_endpoint_index(endpoint);
        let rows = match endpoint_id {
            Some(endpoint_id) => {
                self.query(
                    r#"SELECT record_json, change_set_id
                       FROM contributions
                       WHERE workspace_id = ?
                         AND (
                           (subject_kind = ? AND subject_id = ?)
                           OR (target_kind = ? AND target_id = ?)
                         )
                       ORDER BY updated_at DESC, id DESC"#,
                )
                .bind(workspace_id.0.to_string())
                .bind(endpoint_kind)
                .bind(&endpoint_id)
                .bind(endpoint_kind)
                .bind(&endpoint_id)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                self.query(
                    r#"SELECT record_json, change_set_id
                       FROM contributions
                       WHERE workspace_id = ?
                         AND (
                           (subject_kind = ? AND subject_id IS NULL)
                           OR (target_kind = ? AND target_id IS NULL)
                         )
                       ORDER BY updated_at DESC, id DESC"#,
                )
                .bind(workspace_id.0.to_string())
                .bind(endpoint_kind)
                .bind(endpoint_kind)
                .fetch_all(&self.pool)
                .await?
            }
        };

        rows.into_iter().map(decode_contribution_row).collect()
    }

    async fn contribution_created_at(
        &self,
        workspace_id: WorkspaceId,
        id: ContributionId,
    ) -> Result<Option<DateTime<Utc>>> {
        let raw = self
            .query_scalar::<String>(
                r#"SELECT created_at FROM contributions WHERE id = ? AND workspace_id = ?"#,
            )
            .bind(id.0.to_string())
            .bind(workspace_id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;

        raw.as_deref().map(parse_dt).transpose()
    }

    async fn validate_change_set_workspace(&self, record: &ChangeSet) -> Result<()> {
        if let Some(worktree_id) = record.source_worktree_id {
            let workspace_id = self
                .query_scalar::<String>(r#"SELECT workspace_id FROM worktrees WHERE id = ?"#)
                .bind(worktree_id.0.to_string())
                .fetch_optional(&self.pool)
                .await?;
            match workspace_id {
                Some(workspace_id) if workspace_id == record.workspace_id.0.to_string() => {}
                Some(_) => {
                    return Err(anyhow::anyhow!(
                        "change set source worktree belongs to a different workspace"
                    ));
                }
                None => return Err(anyhow::anyhow!("change set source worktree does not exist")),
            }
        }
        Ok(())
    }

    async fn validate_contribution_workspace(&self, record: &Contribution) -> Result<()> {
        if let Some(change_set_id) = record.change_set_id.as_ref() {
            self.validate_id_workspace(
                "change_sets",
                change_set_id.0.to_string(),
                record.workspace_id,
                "contribution change set",
            )
            .await?;
        }
        self.validate_endpoint_workspace(record.workspace_id, &record.subject, "subject")
            .await?;
        self.validate_endpoint_workspace(record.workspace_id, &record.target, "target")
            .await?;
        Ok(())
    }

    async fn validate_endpoint_workspace(
        &self,
        workspace_id: WorkspaceId,
        endpoint: &ContributionEndpoint,
        label: &str,
    ) -> Result<()> {
        match endpoint {
            ContributionEndpoint::Workspace {
                workspace_id: endpoint_workspace_id,
            } if *endpoint_workspace_id != workspace_id => Err(anyhow::anyhow!(
                "contribution {label} workspace belongs to a different workspace"
            )),
            ContributionEndpoint::Workspace { .. }
            | ContributionEndpoint::Account { .. }
            | ContributionEndpoint::PullRequest { .. }
            | ContributionEndpoint::Check { .. }
            | ContributionEndpoint::Evidence { .. }
            | ContributionEndpoint::ReviewAttestation { .. }
            | ContributionEndpoint::Commit { .. }
            | ContributionEndpoint::Branch { .. }
            | ContributionEndpoint::System { .. } => Ok(()),
            ContributionEndpoint::Task {
                task_id: Some(task_id),
                ..
            } => {
                self.validate_id_workspace(
                    "tasks",
                    task_id.0.to_string(),
                    workspace_id,
                    &format!("contribution {label} task"),
                )
                .await
            }
            ContributionEndpoint::Task { task_id: None, id } => {
                validate_external_endpoint_id(id, &format!("contribution {label} task"))
            }
            ContributionEndpoint::Session {
                session_id: Some(session_id),
                turn_id,
                run_id,
                ..
            } => {
                self.validate_id_workspace(
                    "sessions",
                    session_id.0.to_string(),
                    workspace_id,
                    &format!("contribution {label} session"),
                )
                .await?;
                if let Some(run_id) = run_id {
                    self.validate_id_workspace(
                        "runs",
                        run_id.0.to_string(),
                        workspace_id,
                        &format!("contribution {label} run"),
                    )
                    .await?;
                    self.validate_run_session(
                        *run_id,
                        *session_id,
                        &format!("contribution {label} run/session"),
                    )
                    .await?;
                }
                if let Some(turn_id) = turn_id {
                    self.validate_turn_session(
                        *turn_id,
                        *session_id,
                        *run_id,
                        &format!("contribution {label} turn/session"),
                    )
                    .await?;
                }
                Ok(())
            }
            ContributionEndpoint::Session {
                session_id: None,
                id,
                ..
            } => validate_external_endpoint_id(id, &format!("contribution {label} session")),
            ContributionEndpoint::Run {
                run_id: Some(run_id),
                session_id,
                ..
            } => {
                self.validate_id_workspace(
                    "runs",
                    run_id.0.to_string(),
                    workspace_id,
                    &format!("contribution {label} run"),
                )
                .await?;
                if let Some(session_id) = session_id {
                    self.validate_id_workspace(
                        "sessions",
                        session_id.0.to_string(),
                        workspace_id,
                        &format!("contribution {label} session"),
                    )
                    .await?;
                    self.validate_run_session(
                        *run_id,
                        *session_id,
                        &format!("contribution {label} run/session"),
                    )
                    .await?;
                }
                Ok(())
            }
            ContributionEndpoint::Run {
                run_id: None, id, ..
            } => validate_external_endpoint_id(id, &format!("contribution {label} run")),
            ContributionEndpoint::Agent {
                session_id, run_id, ..
            } => {
                if let Some(session_id) = session_id {
                    self.validate_id_workspace(
                        "sessions",
                        session_id.0.to_string(),
                        workspace_id,
                        &format!("contribution {label} agent session"),
                    )
                    .await?;
                }
                if let Some(run_id) = run_id {
                    self.validate_id_workspace(
                        "runs",
                        run_id.0.to_string(),
                        workspace_id,
                        &format!("contribution {label} agent run"),
                    )
                    .await?;
                }
                if let (Some(session_id), Some(run_id)) = (session_id, run_id) {
                    self.validate_run_session(
                        *run_id,
                        *session_id,
                        &format!("contribution {label} agent run/session"),
                    )
                    .await?;
                }
                Ok(())
            }
            ContributionEndpoint::Worktree {
                worktree_id: Some(worktree_id),
                ..
            } => {
                self.validate_id_workspace(
                    "worktrees",
                    worktree_id.0.to_string(),
                    workspace_id,
                    &format!("contribution {label} worktree"),
                )
                .await
            }
            ContributionEndpoint::Worktree {
                worktree_id: None,
                id,
            } => validate_external_endpoint_id(id, &format!("contribution {label} worktree")),
            ContributionEndpoint::ChangeSet { change_set_id } => {
                self.validate_id_workspace(
                    "change_sets",
                    change_set_id.0.to_string(),
                    workspace_id,
                    &format!("contribution {label} change set"),
                )
                .await
            }
            ContributionEndpoint::Artifact {
                artifact_id: Some(artifact_id),
                ..
            } => {
                self.validate_id_workspace(
                    "artifacts",
                    artifact_id.0.to_string(),
                    workspace_id,
                    &format!("contribution {label} artifact"),
                )
                .await
            }
            ContributionEndpoint::Artifact {
                artifact_id: None,
                digest,
                relative_path,
            } => validate_artifact_endpoint_identity(digest, relative_path, label),
            ContributionEndpoint::External {
                source,
                identifier,
                url,
            } => validate_external_endpoint_identity(source, identifier, url, label),
            ContributionEndpoint::File {
                path,
                worktree_id: Some(worktree_id),
                ..
            } => {
                validate_file_endpoint_path(path, label)?;
                self.validate_id_workspace(
                    "worktrees",
                    worktree_id.0.to_string(),
                    workspace_id,
                    &format!("contribution {label} file worktree"),
                )
                .await
            }
            ContributionEndpoint::File {
                path,
                worktree_id: None,
                ..
            } => validate_file_endpoint_path(path, label),
        }
    }

    async fn validate_id_workspace(
        &self,
        table: &'static str,
        id: String,
        workspace_id: WorkspaceId,
        label: &str,
    ) -> Result<()> {
        let found_workspace_id = match table {
            "artifacts" => {
                self.query_scalar::<String>("SELECT workspace_id FROM artifacts WHERE id = ?")
                    .bind(id)
                    .fetch_optional(&self.pool)
                    .await?
            }
            "change_sets" => {
                self.query_scalar::<String>("SELECT workspace_id FROM change_sets WHERE id = ?")
                    .bind(id)
                    .fetch_optional(&self.pool)
                    .await?
            }
            "runs" => {
                self.query_scalar::<String>("SELECT workspace_id FROM runs WHERE id = ?")
                    .bind(id)
                    .fetch_optional(&self.pool)
                    .await?
            }
            "sessions" => {
                self.query_scalar::<String>("SELECT workspace_id FROM sessions WHERE id = ?")
                    .bind(id)
                    .fetch_optional(&self.pool)
                    .await?
            }
            "tasks" => {
                self.query_scalar::<String>("SELECT workspace_id FROM tasks WHERE id = ?")
                    .bind(id)
                    .fetch_optional(&self.pool)
                    .await?
            }
            "worktrees" => {
                self.query_scalar::<String>("SELECT workspace_id FROM worktrees WHERE id = ?")
                    .bind(id)
                    .fetch_optional(&self.pool)
                    .await?
            }
            _ => unreachable!("unsupported workspace-owned endpoint table"),
        };
        match found_workspace_id {
            Some(found_workspace_id) if found_workspace_id == workspace_id.0.to_string() => Ok(()),
            Some(_) => Err(anyhow::anyhow!("{label} belongs to a different workspace")),
            None => Err(anyhow::anyhow!("{label} does not exist")),
        }
    }

    async fn validate_run_session(
        &self,
        run_id: RunId,
        session_id: SessionId,
        label: &str,
    ) -> Result<()> {
        let found_session_id = self
            .query_scalar::<String>("SELECT session_id FROM runs WHERE id = ?")
            .bind(run_id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;
        match found_session_id {
            Some(found_session_id) if found_session_id == session_id.0.to_string() => Ok(()),
            Some(_) => Err(anyhow::anyhow!("{label} points at different sessions")),
            None => Err(anyhow::anyhow!("{label} run does not exist")),
        }
    }

    async fn validate_turn_session(
        &self,
        turn_id: TurnId,
        session_id: SessionId,
        run_id: Option<RunId>,
        label: &str,
    ) -> Result<()> {
        let row = self
            .query(r#"SELECT session_id, run_id FROM session_turns WHERE turn_id = ?"#)
            .bind(turn_id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;
        let Some(row) = row else {
            return Err(anyhow::anyhow!("{label} turn does not exist"));
        };

        let found_session_id: String = row.try_get("session_id")?;
        if found_session_id != session_id.0.to_string() {
            return Err(anyhow::anyhow!("{label} points at different sessions"));
        }

        if let Some(run_id) = run_id {
            let found_run_id: Option<String> = row.try_get("run_id")?;
            match found_run_id {
                Some(found_run_id) if found_run_id == run_id.0.to_string() => {}
                Some(_) => return Err(anyhow::anyhow!("{label} points at different runs")),
                None => return Err(anyhow::anyhow!("{label} turn has no run")),
            }
        }

        Ok(())
    }
}

fn decode_change_set_row(row: SqliteRow) -> Result<ChangeSet> {
    let record_json: String = row.try_get("record_json")?;
    let mut record: ChangeSet =
        serde_json::from_str(&record_json).context("decoding change set record")?;
    record.source_worktree_id = parse_optional_id(row.try_get("source_worktree_id")?)?;
    validate_agent_work_schema_version(record.schema_version, "change set")?;
    validate_agent_work_source_records(&record.source_records, "change set")?;
    Ok(record)
}

fn decode_contribution_row(row: SqliteRow) -> Result<Contribution> {
    let record_json: String = row.try_get("record_json")?;
    let mut record: Contribution =
        serde_json::from_str(&record_json).context("decoding contribution record")?;
    record.change_set_id = parse_optional_string_id(row.try_get("change_set_id")?)?;
    validate_agent_work_schema_version(record.schema_version, "contribution")?;
    validate_agent_work_source_records(&record.source_records, "contribution")?;
    Ok(record)
}

fn validate_agent_work_schema_version(schema_version: i64, label: &str) -> Result<()> {
    if schema_version != AGENT_WORK_EXPORT_SCHEMA_VERSION {
        anyhow::bail!(
            "{label} schema_version {} is not supported; expected {}",
            schema_version,
            AGENT_WORK_EXPORT_SCHEMA_VERSION
        );
    }
    Ok(())
}

fn validate_agent_work_source_records(
    source_records: &[AgentWorkSourceRecord],
    label: &str,
) -> Result<()> {
    for source_record in source_records {
        validate_agent_work_schema_version(source_record.schema_version, "source record")?;
        let hash_matches = source_record
            .verify_record_hash()
            .context("verifying agent work source record hash")?;
        if !hash_matches {
            anyhow::bail!(
                "{label} source record {} has an invalid record_hash",
                source_record.record_id.0
            );
        }
    }
    Ok(())
}

fn validate_external_endpoint_id(id: &Option<String>, label: &str) -> Result<()> {
    match id.as_deref().map(str::trim).filter(|id| !id.is_empty()) {
        Some(_) => Ok(()),
        None => Err(anyhow::anyhow!(
            "{label} is missing a local id or external id"
        )),
    }
}

fn validate_artifact_endpoint_identity(
    digest: &Option<String>,
    relative_path: &Option<String>,
    label: &str,
) -> Result<()> {
    match (
        trimmed_endpoint_component(digest),
        trimmed_endpoint_component(relative_path),
    ) {
        (None, None) => Err(anyhow::anyhow!(
            "contribution {label} artifact is missing artifact_id, digest, or relative_path"
        )),
        _ => Ok(()),
    }
}

fn validate_external_endpoint_identity(
    source: &str,
    identifier: &Option<String>,
    url: &Option<String>,
    label: &str,
) -> Result<()> {
    if source.trim().is_empty() {
        anyhow::bail!("contribution {label} external endpoint is missing source");
    }
    match (
        trimmed_endpoint_component(identifier),
        trimmed_endpoint_component(url),
    ) {
        (None, None) => Err(anyhow::anyhow!(
            "contribution {label} external endpoint is missing identifier or url"
        )),
        _ => Ok(()),
    }
}

fn validate_file_endpoint_path(path: &str, label: &str) -> Result<()> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        anyhow::bail!("contribution {label} file endpoint is missing path");
    }
    if Path::new(trimmed).is_absolute()
        || trimmed.starts_with("\\\\")
        || looks_like_windows_absolute_path(trimmed)
    {
        anyhow::bail!("contribution {label} file path must be workspace-relative");
    }
    Ok(())
}

fn looks_like_windows_absolute_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
}

fn external_endpoint_index_key(id: &Option<String>) -> Option<String> {
    trimmed_endpoint_component(id)
}

fn trimmed_endpoint_component(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn external_session_endpoint_index_key(
    provider: &Option<String>,
    id: &Option<String>,
) -> Option<String> {
    let id = external_endpoint_index_key(id)?;
    let provider = provider
        .as_deref()
        .map(str::trim)
        .filter(|provider| !provider.is_empty())
        .unwrap_or_default()
        .to_string();
    Some(endpoint_index_key(&[("provider", provider), ("id", id)]))
}

fn contribution_endpoint_index(endpoint: &ContributionEndpoint) -> (&'static str, Option<String>) {
    match endpoint {
        ContributionEndpoint::Account { account_id } => ("account", Some(account_id.0.to_string())),
        ContributionEndpoint::Workspace { workspace_id } => {
            ("workspace", Some(workspace_id.0.to_string()))
        }
        ContributionEndpoint::Task {
            task_id: Some(task_id),
            ..
        } => ("task", Some(task_id.0.to_string())),
        ContributionEndpoint::Task { task_id: None, id } => {
            ("task", external_endpoint_index_key(id))
        }
        ContributionEndpoint::Session {
            session_id: Some(session_id),
            turn_id,
            run_id,
            ..
        } => (
            "session",
            Some(endpoint_index_key(&[
                ("session_id", session_id.0.to_string()),
                (
                    "turn_id",
                    turn_id.map(|id| id.0.to_string()).unwrap_or_default(),
                ),
                (
                    "run_id",
                    run_id.map(|id| id.0.to_string()).unwrap_or_default(),
                ),
            ])),
        ),
        ContributionEndpoint::Session {
            session_id: None,
            provider,
            id,
            ..
        } => ("session", external_session_endpoint_index_key(provider, id)),
        ContributionEndpoint::Run {
            run_id: Some(run_id),
            session_id,
            ..
        } => (
            "run",
            Some(endpoint_index_key(&[
                ("run_id", run_id.0.to_string()),
                (
                    "session_id",
                    session_id.map(|id| id.0.to_string()).unwrap_or_default(),
                ),
            ])),
        ),
        ContributionEndpoint::Run {
            run_id: None, id, ..
        } => ("run", external_endpoint_index_key(id)),
        ContributionEndpoint::Agent {
            run_id,
            session_id,
            label,
        } => (
            "agent",
            agent_endpoint_index_key(*run_id, *session_id, label.as_deref()),
        ),
        ContributionEndpoint::System { label } => ("system", label.clone()),
        ContributionEndpoint::Worktree {
            worktree_id: Some(worktree_id),
            ..
        } => ("worktree", Some(worktree_id.0.to_string())),
        ContributionEndpoint::Worktree {
            worktree_id: None,
            id,
        } => ("worktree", external_endpoint_index_key(id)),
        ContributionEndpoint::ChangeSet { change_set_id } => {
            ("change_set", Some(change_set_id.0.to_string()))
        }
        ContributionEndpoint::PullRequest { pull_request } => {
            ("pull_request", Some(pull_request_index_key(pull_request)))
        }
        ContributionEndpoint::Artifact {
            artifact_id,
            digest,
            relative_path,
        } => (
            "artifact",
            artifact_id
                .as_ref()
                .map(|id| endpoint_index_key(&[("artifact_id", id.0.to_string())]))
                .or_else(|| artifact_endpoint_index_key(digest, relative_path)),
        ),
        ContributionEndpoint::Check { check_id } => ("check", Some(check_id.clone())),
        ContributionEndpoint::Evidence { id } => ("evidence", Some(id.clone())),
        ContributionEndpoint::ReviewAttestation { id } => ("review_attestation", Some(id.clone())),
        ContributionEndpoint::Commit { sha } => ("commit", Some(sha.clone())),
        ContributionEndpoint::Branch { name } => ("branch", Some(name.clone())),
        ContributionEndpoint::File { path, worktree_id } => (
            "file",
            Some(endpoint_index_key(&[
                (
                    "worktree_id",
                    worktree_id.map(|id| id.0.to_string()).unwrap_or_default(),
                ),
                ("path", path.clone()),
            ])),
        ),
        ContributionEndpoint::External {
            source,
            identifier,
            url,
        } => (
            "external",
            external_contribution_endpoint_index_key(source, identifier, url),
        ),
    }
}

fn artifact_endpoint_index_key(
    digest: &Option<String>,
    relative_path: &Option<String>,
) -> Option<String> {
    let digest = trimmed_endpoint_component(digest);
    let relative_path = trimmed_endpoint_component(relative_path);
    if digest.is_none() && relative_path.is_none() {
        return None;
    }
    Some(endpoint_index_key(&[
        ("digest", digest.unwrap_or_default()),
        ("relative_path", relative_path.unwrap_or_default()),
    ]))
}

fn external_contribution_endpoint_index_key(
    source: &str,
    identifier: &Option<String>,
    url: &Option<String>,
) -> Option<String> {
    let source = source.trim();
    if source.is_empty() {
        return None;
    }
    let identifier = trimmed_endpoint_component(identifier);
    let url = trimmed_endpoint_component(url);
    if identifier.is_none() && url.is_none() {
        return None;
    }
    Some(endpoint_index_key(&[
        ("source", source.to_string()),
        ("identifier", identifier.unwrap_or_default()),
        ("url", url.unwrap_or_default()),
    ]))
}

fn parse_optional_id<T>(raw: Option<String>) -> Result<Option<T>>
where
    T: From<uuid::Uuid>,
{
    raw.map(|raw| uuid::Uuid::parse_str(&raw).map(T::from))
        .transpose()
        .context("decoding agent work id")
}

fn parse_optional_string_id<T>(raw: Option<String>) -> Result<Option<T>>
where
    T: From<String>,
{
    Ok(raw.map(T::from))
}

fn pull_request_index_key(pull_request: &PullRequestRef) -> String {
    endpoint_index_key(&[
        ("provider", pull_request.provider.clone()),
        ("owner", pull_request.owner.clone()),
        ("repo", pull_request.repo.clone()),
        ("number", pull_request.number.to_string()),
    ])
}

fn agent_endpoint_index_key(
    run_id: Option<RunId>,
    session_id: Option<SessionId>,
    label: Option<&str>,
) -> Option<String> {
    match (run_id, session_id, label) {
        (None, None, None) => None,
        _ => Some(endpoint_index_key(&[
            (
                "run_id",
                run_id.map(|id| id.0.to_string()).unwrap_or_default(),
            ),
            (
                "session_id",
                session_id.map(|id| id.0.to_string()).unwrap_or_default(),
            ),
            ("label", label.unwrap_or_default().to_string()),
        ])),
    }
}

fn endpoint_index_key(parts: &[(&str, String)]) -> String {
    serde_json::to_string(parts).expect("endpoint index key should serialize")
}
