use super::*;

pub(crate) async fn list_session_subagents(
    State(state): State<SessionSubagentReadHandle>,
    Path(id): Path<String>,
) -> Result<Json<SessionSubagentsRouteResponse>, StatusCode> {
    state
        .list_session_subagents_for_route(SessionRouteParams::new(id))
        .await
        .map_err(subagent_bare_status)
        .map(Json)
}

pub(crate) async fn list_session_subagent_invocations(
    State(state): State<SessionSubagentReadHandle>,
    Path(id): Path<String>,
    Query(q): Query<SessionSubagentInvocationsRouteQuery>,
) -> Result<Json<SessionSubagentInvocationsRouteResponse>, StatusCode> {
    state
        .list_session_subagent_invocations_for_route(SessionRouteParams::new(id), q)
        .await
        .map_err(subagent_bare_status)
        .map(Json)
}

pub(crate) async fn get_session_subagent_invocation(
    State(state): State<SessionSubagentReadHandle>,
    Path((session_id, id)): Path<(String, String)>,
) -> Result<Json<SessionSubagentInvocationRouteResponse>, StatusCode> {
    state
        .get_session_subagent_invocation_for_route(SessionRouteParams::new(session_id), id)
        .await
        .map_err(subagent_bare_status)
        .map(Json)
}
