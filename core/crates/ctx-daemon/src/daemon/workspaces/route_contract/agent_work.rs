use ctx_core::ids::ChangeSetId;
use ctx_core::models::{Contribution, ContributionEndpoint};
use ctx_route_contracts::workspaces::{
    WorkspaceAgentWorkRouteQuery, WorkspaceAgentWorkRouteResponse, WorkspaceRouteParams,
};
use std::collections::HashSet;

use super::super::{workspace_store_route_error, WorkspaceRouteError};
use crate::daemon::WorkspaceAgentWorkHandle;

impl WorkspaceAgentWorkHandle {
    pub async fn list_workspace_agent_work_for_route(
        &self,
        params: WorkspaceRouteParams,
        query: WorkspaceAgentWorkRouteQuery,
    ) -> Result<WorkspaceAgentWorkRouteResponse, WorkspaceRouteError> {
        let workspace_id = params.parse_workspace_id()?;
        let store = self
            .existing_workspace_store(workspace_id)
            .await
            .map_err(workspace_store_route_error)?;
        let limit = query.limit.unwrap_or(usize::MAX).min(5_000);
        let (mut change_sets, mut contributions) = if let Some(change_set_id) = query
            .change_set_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
        {
            let change_set_id = ChangeSetId(change_set_id.to_string());
            load_endpoint_graph(
                &store,
                workspace_id,
                &ContributionEndpoint::ChangeSet { change_set_id },
            )
            .await?
        } else if let Some(endpoint_json) = query
            .endpoint_json
            .as_deref()
            .map(str::trim)
            .filter(|endpoint| !endpoint.is_empty())
        {
            let endpoint =
                serde_json::from_str::<ContributionEndpoint>(endpoint_json).map_err(|error| {
                    WorkspaceRouteError::bad_request(format!("invalid endpoint_json: {error}"))
                })?;
            load_endpoint_graph(&store, workspace_id, &endpoint).await?
        } else {
            let change_sets = store
                .list_workspace_change_sets(workspace_id)
                .await
                .map_err(WorkspaceRouteError::internal)?;
            let contributions = store
                .list_workspace_contributions(workspace_id)
                .await
                .map_err(WorkspaceRouteError::internal)?;
            (change_sets, contributions)
        };
        change_sets.truncate(limit);
        contributions.truncate(limit);
        Ok(WorkspaceAgentWorkRouteResponse::new(
            change_sets,
            contributions,
        ))
    }
}

async fn load_endpoint_graph(
    store: &ctx_store::Store,
    workspace_id: ctx_core::ids::WorkspaceId,
    endpoint: &ContributionEndpoint,
) -> Result<(Vec<ctx_core::models::ChangeSet>, Vec<Contribution>), WorkspaceRouteError> {
    if let ContributionEndpoint::ChangeSet { change_set_id } = endpoint {
        let mut contributions = store
            .list_contributions_for_change_set(workspace_id, change_set_id.clone())
            .await
            .map_err(WorkspaceRouteError::internal)?;
        contributions.extend(
            store
                .list_contributions_for_endpoint(workspace_id, endpoint)
                .await
                .map_err(WorkspaceRouteError::internal)?,
        );
        dedupe_contributions(&mut contributions);
        let mut change_sets =
            load_change_sets_for_contributions(store, workspace_id, &contributions).await?;
        let change_set = store
            .get_workspace_change_set(workspace_id, change_set_id.clone())
            .await
            .map_err(WorkspaceRouteError::internal)?;
        if let Some(change_set) = change_set {
            if !change_sets
                .iter()
                .any(|existing| existing.id == change_set.id)
            {
                change_sets.push(change_set);
            }
        }
        return Ok((change_sets, contributions));
    }

    let contributions = store
        .list_contributions_for_endpoint(workspace_id, endpoint)
        .await
        .map_err(WorkspaceRouteError::internal)?;
    let change_sets =
        load_change_sets_for_contributions(store, workspace_id, &contributions).await?;
    Ok((change_sets, contributions))
}

async fn load_change_sets_for_contributions(
    store: &ctx_store::Store,
    workspace_id: ctx_core::ids::WorkspaceId,
    contributions: &[Contribution],
) -> Result<Vec<ctx_core::models::ChangeSet>, WorkspaceRouteError> {
    let mut ids = contributions
        .iter()
        .flat_map(contribution_change_set_ids)
        .collect::<Vec<_>>();
    ids.sort_by(|left, right| left.0.cmp(&right.0));
    ids.dedup_by(|left, right| left.0 == right.0);

    let mut change_sets = Vec::new();
    for id in ids {
        if let Some(change_set) = store
            .get_workspace_change_set(workspace_id, id)
            .await
            .map_err(WorkspaceRouteError::internal)?
        {
            change_sets.push(change_set);
        }
    }
    Ok(change_sets)
}

fn dedupe_contributions(contributions: &mut Vec<Contribution>) {
    let mut seen = HashSet::new();
    contributions.retain(|contribution| seen.insert(contribution.id.clone()));
}

fn contribution_change_set_ids(contribution: &Contribution) -> Vec<ChangeSetId> {
    let mut ids = Vec::new();
    if let Some(id) = contribution.change_set_id.clone() {
        ids.push(id);
    }
    push_endpoint_change_set_id(&mut ids, &contribution.subject);
    push_endpoint_change_set_id(&mut ids, &contribution.target);
    ids
}

fn push_endpoint_change_set_id(ids: &mut Vec<ChangeSetId>, endpoint: &ContributionEndpoint) {
    if let ContributionEndpoint::ChangeSet { change_set_id } = endpoint {
        ids.push(change_set_id.clone());
    }
}
