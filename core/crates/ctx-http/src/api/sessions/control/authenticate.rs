use super::super::*;

pub(crate) async fn authenticate_session(
    State(state): State<SessionControlHandle>,
    Path(id): Path<String>,
    Json(req): Json<AuthenticateSessionRouteRequest>,
) -> Result<StatusCode, (StatusCode, Json<ApiErrorResp>)> {
    state
        .authenticate_session_for_route(SessionRouteParams::new(id), req)
        .await
        .map_err(session_control_api_error)?;
    Ok(StatusCode::OK)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_control_api_error_maps_policy_denials_to_forbidden() {
        let (status, body) = session_control_api_error(SessionControlRouteError::new(
            SessionControlRouteErrorKind::Forbidden,
            "host execution is disabled by daemon policy",
        ));

        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(
            body.0.error,
            "host execution is disabled by daemon policy".to_string()
        );
    }
}
