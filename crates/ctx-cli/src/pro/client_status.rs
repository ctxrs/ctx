use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use anyhow::Result;
use ctx_pro_host_protocol::{
    Capability, EntitlementAccessState, GraphState, HelperMessage, HostMessage, StatusRequest,
    StatusResult, PROTOCOL_FINGERPRINT, PROTOCOL_VERSION,
};
use serde::Serialize;

use super::{
    default_helper_path, helper_status, protocol_error, support, AuthorizationProvider, ProClient,
    HANDSHAKE_TIMEOUT,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProSetupRepairability {
    NotNeeded,
    Automated,
    ManualDiagnosis,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProStatus {
    pub(crate) schema_version: u32,
    pub(crate) installed: bool,
    pub(crate) ready: bool,
    pub(crate) materialized: bool,
    pub(crate) helper_path: PathBuf,
    pub(crate) helper_version: Option<String>,
    pub(crate) protocol_version: u16,
    pub(crate) capabilities: Vec<String>,
    pub(crate) error_code: Option<String>,
    pub(crate) access_state: Option<String>,
    pub(crate) refresh_after_unix: Option<i64>,
    pub(crate) access_deadline_unix: Option<i64>,
    pub(crate) grace_deadline_unix: Option<i64>,
    #[serde(skip)]
    pub(crate) setup_repairability: ProSetupRepairability,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HelperSmoke {
    pub(crate) protocol_version: u16,
    pub(crate) protocol_fingerprint: String,
    pub(crate) helper_version: String,
    pub(crate) capabilities: BTreeSet<Capability>,
}

/// Starts an explicit staged helper and completes the exact Protocol V1 hello.
/// This does not consult or mutate the installed-helper lifecycle state.
pub(crate) fn smoke_helper_at_path(data_root: &Path, path: &Path) -> Result<HelperSmoke> {
    smoke_helper_at_path_with_authorization(data_root, path, None)
}

pub(super) fn smoke_helper_at_path_with_authorization(
    data_root: &Path,
    path: &Path,
    authorization: Option<&dyn AuthorizationProvider>,
) -> Result<HelperSmoke> {
    smoke_helper_at_path_with_authorization_observing_status(data_root, path, authorization, drop)
}

fn smoke_helper_at_path_with_authorization_observing_status(
    data_root: &Path,
    path: &Path,
    authorization: Option<&dyn AuthorizationProvider>,
    observe_status: impl FnOnce(StatusResult),
) -> Result<HelperSmoke> {
    let required = BTreeSet::from([Capability::EntitlementAuthorization, Capability::Status]);
    let mut client = ProClient::connect_to_path_with_authorization_mode(
        data_root,
        path,
        None,
        &required,
        authorization,
        true,
    )?;
    let status = helper_status(&mut client)?;
    let smoke = HelperSmoke {
        protocol_version: PROTOCOL_VERSION,
        protocol_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
        helper_version: client.helper_version.clone(),
        capabilities: client.capabilities.clone(),
    };
    observe_status(status);
    Ok(smoke)
}

#[cfg(all(test, unix))]
pub(super) fn smoke_helper_at_path_with_authorization_and_status(
    data_root: &Path,
    path: &Path,
    authorization: Option<&dyn AuthorizationProvider>,
) -> Result<(HelperSmoke, StatusResult)> {
    let mut observed_status = None;
    let smoke = smoke_helper_at_path_with_authorization_observing_status(
        data_root,
        path,
        authorization,
        |status| observed_status = Some(status),
    )?;
    Ok((
        smoke,
        observed_status.expect("successful helper smoke must observe status"),
    ))
}

pub(crate) fn status(data_root: &Path) -> ProStatus {
    status_with_helper_resolver(data_root, support::helper_path)
}

pub(crate) fn status_with_helper_resolver(
    data_root: &Path,
    resolve_helper: impl FnOnce(&Path) -> Result<PathBuf>,
) -> ProStatus {
    let helper_path = match resolve_helper(data_root) {
        Ok(path) => path,
        Err(error) => {
            let error_code = support::error_code(&error);
            let setup_repairability =
                if crate::pro::lifecycle::is_setup_repair_required_error(&error) {
                    ProSetupRepairability::Automated
                } else if error_code == "pro_not_installed" {
                    ProSetupRepairability::NotNeeded
                } else {
                    ProSetupRepairability::ManualDiagnosis
                };
            return ProStatus {
                schema_version: 1,
                installed: false,
                ready: false,
                materialized: false,
                helper_path: default_helper_path(data_root),
                helper_version: None,
                protocol_version: PROTOCOL_VERSION,
                capabilities: Vec::new(),
                error_code: Some(error_code),
                access_state: None,
                refresh_after_unix: None,
                access_deadline_unix: None,
                grace_deadline_unix: None,
                setup_repairability,
            };
        }
    };
    match ProClient::connect_for_status(data_root, &BTreeSet::from([Capability::Status])) {
        Ok(mut client) => {
            let helper_version = Some(client.helper_version.clone());
            let capabilities = client
                .capabilities
                .iter()
                .map(|capability| capability.wire_name().to_owned())
                .collect();
            let access = client.public_access_status();
            match client.exchange(HostMessage::Status(StatusRequest {}), HANDSHAKE_TIMEOUT) {
                Ok(HelperMessage::Status(result)) => {
                    let (ready, materialized, state_error) =
                        status_outcome(result.state, client.authorization_state);
                    ProStatus {
                        schema_version: 1,
                        installed: true,
                        ready,
                        materialized,
                        helper_path,
                        helper_version,
                        protocol_version: PROTOCOL_VERSION,
                        capabilities,
                        error_code: state_error.map(ToOwned::to_owned),
                        access_state: access.state,
                        refresh_after_unix: access.refresh_after_unix,
                        access_deadline_unix: access.access_deadline_unix,
                        grace_deadline_unix: access.grace_deadline_unix,
                        setup_repairability: ProSetupRepairability::NotNeeded,
                    }
                }
                Ok(HelperMessage::Error(error)) => ProStatus {
                    schema_version: 1,
                    installed: true,
                    ready: false,
                    materialized: false,
                    helper_path,
                    helper_version,
                    protocol_version: PROTOCOL_VERSION,
                    capabilities,
                    error_code: Some(support::error_code(&protocol_error(error))),
                    access_state: access.state,
                    refresh_after_unix: access.refresh_after_unix,
                    access_deadline_unix: access.access_deadline_unix,
                    grace_deadline_unix: access.grace_deadline_unix,
                    setup_repairability: ProSetupRepairability::NotNeeded,
                },
                _ => ProStatus {
                    schema_version: 1,
                    installed: true,
                    ready: false,
                    materialized: false,
                    helper_path,
                    helper_version,
                    protocol_version: PROTOCOL_VERSION,
                    capabilities,
                    error_code: Some("protocol_mismatch".to_owned()),
                    access_state: access.state,
                    refresh_after_unix: access.refresh_after_unix,
                    access_deadline_unix: access.access_deadline_unix,
                    grace_deadline_unix: access.grace_deadline_unix,
                    setup_repairability: ProSetupRepairability::NotNeeded,
                },
            }
        }
        Err(error) => ProStatus {
            schema_version: 1,
            installed: true,
            ready: false,
            materialized: false,
            helper_path,
            helper_version: None,
            protocol_version: PROTOCOL_VERSION,
            capabilities: Vec::new(),
            error_code: Some(support::error_code(&error)),
            access_state: None,
            refresh_after_unix: None,
            access_deadline_unix: None,
            grace_deadline_unix: None,
            setup_repairability: ProSetupRepairability::NotNeeded,
        },
    }
}

pub(super) fn status_outcome(
    state: GraphState,
    authorization_state: Option<EntitlementAccessState>,
) -> (bool, bool, Option<&'static str>) {
    let materialized = state == GraphState::Ready;
    if authorization_state == Some(EntitlementAccessState::Locked) {
        return (false, materialized, Some("entitlement_expired"));
    }
    let error = match state {
        GraphState::NotMaterialized => Some("not_materialized"),
        GraphState::NeedsRebuild => Some("needs_rebuild"),
        GraphState::Partial => Some("partial"),
        GraphState::NeedsResume => Some("needs_resume"),
        GraphState::Ready => None,
    };
    (materialized, materialized, error)
}
