use super::super::ProviderAccountMutationError;
use ctx_provider_accounts::ProviderAccountRouteError;

pub(in crate::daemon::providers::accounts::routes) fn internal_error(
    error: impl ToString,
) -> ProviderAccountRouteError {
    ProviderAccountRouteError::internal(error.to_string())
}

fn unknown_account_error() -> ProviderAccountRouteError {
    ProviderAccountRouteError::not_found("unknown account")
}

fn provider_account_delete_error(err: anyhow::Error) -> ProviderAccountRouteError {
    let error = err.to_string();
    if error.contains("unknown account") {
        ProviderAccountRouteError::not_found(error)
    } else {
        ProviderAccountRouteError::internal(error)
    }
}

pub(in crate::daemon::providers::accounts::routes) fn provider_account_mutation_error(
    err: ProviderAccountMutationError,
) -> ProviderAccountRouteError {
    match err {
        ProviderAccountMutationError::BadRequest(err) => {
            ProviderAccountRouteError::bad_request(err.to_string())
        }
        ProviderAccountMutationError::Delete(err) => provider_account_delete_error(err),
        ProviderAccountMutationError::Internal(err) => internal_error(err),
    }
}

pub(in crate::daemon::providers::accounts::routes) fn codex_set_active_error(
    err: ProviderAccountMutationError,
) -> ProviderAccountRouteError {
    match err {
        ProviderAccountMutationError::BadRequest(err) => {
            let msg = err.to_string();
            if msg.contains("api_shape=openai_responses")
                || msg.contains("auth_type=bearer")
                || msg.contains("unknown account")
            {
                ProviderAccountRouteError::bad_request(msg)
            } else {
                ProviderAccountRouteError::internal(msg)
            }
        }
        ProviderAccountMutationError::Delete(err) => provider_account_delete_error(err),
        ProviderAccountMutationError::Internal(err) => internal_error(err),
    }
}

pub(in crate::daemon::providers::accounts::routes) fn ensure_known_account<T>(
    account_id: &Option<String>,
    accounts: &[T],
    id: impl Fn(&T) -> &str,
) -> Result<(), ProviderAccountRouteError> {
    let Some(account_id) = account_id.as_ref() else {
        return Ok(());
    };
    if accounts.iter().any(|account| id(account) == account_id) {
        return Ok(());
    }

    Err(unknown_account_error())
}
