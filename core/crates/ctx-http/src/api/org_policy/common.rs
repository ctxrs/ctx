use super::*;

pub(super) fn policy_api_error(error: OrgPolicyRouteError) -> (StatusCode, Json<ApiErrorResp>) {
    let status = match error.kind() {
        OrgPolicyRouteErrorKind::BadRequest => StatusCode::BAD_REQUEST,
        OrgPolicyRouteErrorKind::Conflict => StatusCode::CONFLICT,
        OrgPolicyRouteErrorKind::NotFound => StatusCode::NOT_FOUND,
        OrgPolicyRouteErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (
        status,
        Json(ApiErrorResp {
            error: error.message().to_string(),
        }),
    )
}
