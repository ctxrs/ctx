use crate::api::router::RouteState;

mod accounts;
mod auth_import;
mod base;
mod harness_config;
mod installs;

use accounts::provider_account_routes;
use auth_import::provider_auth_import_routes;
use base::provider_base_routes;
use harness_config::provider_harness_config_routes;
use installs::provider_install_routes;

pub(super) fn provider_routes() -> axum::Router<RouteState> {
    provider_base_routes()
        .merge(provider_harness_config_routes())
        .merge(provider_auth_import_routes())
        .merge(provider_account_routes())
        .merge(provider_install_routes())
}
