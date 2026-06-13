use super::*;

pub(super) fn session_routes() -> axum::Router<RouteState> {
    axum::Router::new()
        .route(
            "/api/sessions/:id/artifacts/:artifact_id",
            get(get_session_artifact),
        )
        .route(
            "/api/tasks/:id/sessions",
            get(list_task_sessions).post(create_session_for_task),
        )
        .route("/api/sessions/:id/messages", post(post_message))
        .route("/api/sessions/:id/subagents", get(list_session_subagents))
        .route(
            "/api/sessions/:id/subagent_invocations",
            get(list_session_subagent_invocations),
        )
        .route(
            "/api/sessions/:id/subagent_invocations/:invocation_id",
            get(get_session_subagent_invocation),
        )
        .route(
            "/api/sessions/:id/artifacts",
            get(list_session_artifacts).post(set_session_artifacts),
        )
        .route("/api/sessions/:id/model", post(set_session_model))
        .route("/api/sessions/:id/mode", post(set_session_mode))
        .route(
            "/api/sessions/:id/title/generate",
            post(generate_session_title),
        )
        .route("/api/sessions/:id/snapshot", get(get_session_snapshot))
        .route("/api/sessions/:id/head", get(get_session_head))
        .route("/api/sessions/:id/state", get(get_session_state))
        .route("/api/sessions/:id/diff", get(get_session_diff))
        .route(
            "/api/sessions/:id/diff/summary",
            get(get_session_diff_summary),
        )
        .route("/api/sessions/:id/git/status", get(get_session_git_status))
        .route(
            "/api/sessions/:id/diff/apply",
            post(apply_session_diff_patch),
        )
        .route("/api/sessions/:id/events", get(get_session_events))
        .route("/api/sessions/:id/history", get(get_session_history))
        .route(
            "/api/sessions/:id/turns/:turn_id/tools",
            get(list_session_turn_tools),
        )
        .route(
            "/api/sessions/:id/completions/files",
            get(session_file_completions),
        )
        .route(
            "/api/sessions/:id/messages/:message_id",
            delete(delete_session_message),
        )
        .route("/api/sessions/:id/cancel", post(cancel_session))
        .route("/api/sessions/:id/interrupt", post(interrupt_session))
        .route("/api/sessions/:id/authenticate", post(authenticate_session))
        .route("/api/mcp/sessions/:id/spawn_agent", post(mcp_spawn_agent))
        .route("/api/mcp/sessions/:id/send_input", post(mcp_send_input))
        .route(
            "/api/mcp/sessions/:id/archive_agent",
            post(mcp_archive_agent),
        )
        .route(
            "/api/mcp/sessions/:id/interrupt_agent",
            post(mcp_interrupt_agent),
        )
        .route("/api/mcp/sessions/:id/list_agents", get(mcp_list_agents))
        .route("/api/mcp/sessions/:id/get_agent", post(mcp_get_agent))
        .route("/api/mcp/sessions/:id/wait_agent", post(mcp_wait_agent))
        .route(
            "/api/sessions/web",
            post(create_web_session).get(list_web_sessions),
        )
        .route("/api/sessions/web/:id", get(get_web_session))
        .route(
            "/api/sessions/web/:id/stream_token",
            post(mint_web_session_stream_token),
        )
        .route("/api/sessions/web/:id/run", post(run_web_session))
        .route("/api/sessions/web/:id/eval", post(eval_web_session))
        .route("/api/sessions/web/:id/close", post(close_web_session))
        .route("/sessions/web/:id/view", get(web_session_view))
        .route("/sessions/web/:id/signal", get(web_session_signal))
        .route(
            "/api/sessions/:id/ask_user_question",
            post(submit_ask_user_question),
        )
}
