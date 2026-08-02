use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use anyhow::Result;
use ctx_history_index::VerifiedIndex;
use ctx_pro_host_protocol::{
    Capability, CoreProjectionCurrentness, EntitlementAccessState, HelperMessage, HostMessage,
    MaterializedCoverage, ProOperation, RepositoryCoverage, StatusRequest, StatusResult,
    PROTOCOL_FINGERPRINT, PROTOCOL_VERSION,
};
use serde::Serialize;

use super::{
    default_helper_path, helper_status, protocol_error, support, AuthorizationProvider, ProClient,
    VerifiedHelperExecutable, HANDSHAKE_TIMEOUT,
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
    pub(crate) projection_currentness: Option<CoreProjectionCurrentness>,
    pub(crate) materialized_coverage: Option<MaterializedCoverage>,
    pub(crate) repository_coverage: Option<RepositoryCoverage>,
    pub(crate) supported_operations: Option<BTreeSet<ProOperation>>,
    pub(crate) available_operations: Option<BTreeSet<ProOperation>>,
    pub(crate) access_state: Option<String>,
    pub(crate) refresh_after_unix: Option<i64>,
    pub(crate) access_deadline_unix: Option<i64>,
    pub(crate) grace_deadline_unix: Option<i64>,
    #[serde(skip)]
    pub(crate) setup_repairability: ProSetupRepairability,
}

enum StatusCore<'a> {
    Borrowed(&'a VerifiedIndex),
    Owned(Box<crate::semantic::PinnedSourceBackedGeneration>),
}

impl StatusCore<'_> {
    fn generation_id(&self) -> &str {
        match self {
            Self::Borrowed(index) => index.generation_id(),
            Self::Owned(index) => index.generation_id(),
        }
    }
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
    smoke_helper_at_path_with_authorization_observing_status(
        data_root,
        path,
        None,
        authorization,
        drop,
    )
}

#[cfg(ctx_pro_qualification)]
pub(crate) fn smoke_qualification_helper(
    data_root: &Path,
    executable: VerifiedHelperExecutable,
) -> Result<HelperSmoke> {
    let path = executable.path().to_path_buf();
    smoke_helper_at_path_with_authorization_observing_status(
        data_root,
        &path,
        Some(executable),
        None,
        drop,
    )
}

fn smoke_helper_at_path_with_authorization_observing_status(
    data_root: &Path,
    path: &Path,
    execution_guard: Option<VerifiedHelperExecutable>,
    authorization: Option<&dyn AuthorizationProvider>,
    observe_status: impl FnOnce(StatusResult),
) -> Result<HelperSmoke> {
    let required = BTreeSet::from([Capability::EntitlementAuthorization, Capability::Status]);
    let mut client = ProClient::connect_to_path_with_authorization_mode(
        data_root,
        path,
        execution_guard,
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

pub(crate) fn status(data_root: &Path) -> ProStatus {
    status_with_helper_resolver(data_root, support::helper_path)
}

pub(crate) fn status_for_core(data_root: &Path, active_core: Option<&VerifiedIndex>) -> ProStatus {
    status_with_helper_resolver_and_core(data_root, support::helper_path, || {
        active_core.map(StatusCore::Borrowed).ok_or_else(|| {
            anyhow::anyhow!("source_unavailable: active verified Core generation is missing")
        })
    })
}

pub(crate) fn status_with_helper_resolver(
    data_root: &Path,
    resolve_helper: impl FnOnce(&Path) -> Result<PathBuf>,
) -> ProStatus {
    status_with_helper_resolver_and_core(data_root, resolve_helper, || {
        crate::semantic::pin_active_verified_generation(data_root)
            .map(|index| StatusCore::Owned(Box::new(index)))
    })
}

fn status_with_helper_resolver_and_core<'a>(
    data_root: &Path,
    resolve_helper: impl FnOnce(&Path) -> Result<PathBuf>,
    resolve_core: impl FnOnce() -> Result<StatusCore<'a>>,
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
                projection_currentness: None,
                materialized_coverage: None,
                repository_coverage: None,
                supported_operations: None,
                available_operations: None,
                access_state: None,
                refresh_after_unix: None,
                access_deadline_unix: None,
                grace_deadline_unix: None,
                setup_repairability,
            };
        }
    };
    let active_core = match resolve_core() {
        Ok(active_core) => active_core,
        Err(error) => {
            return ProStatus {
                schema_version: 1,
                installed: true,
                ready: false,
                materialized: false,
                helper_path,
                helper_version: None,
                protocol_version: PROTOCOL_VERSION,
                capabilities: Vec::new(),
                error_code: Some(support::error_code(&error)),
                projection_currentness: None,
                materialized_coverage: None,
                repository_coverage: None,
                supported_operations: None,
                available_operations: None,
                access_state: None,
                refresh_after_unix: None,
                access_deadline_unix: None,
                grace_deadline_unix: None,
                setup_repairability: ProSetupRepairability::NotNeeded,
            };
        }
    };
    let active_core_generation_id = active_core.generation_id();
    match ProClient::connect_for_status(data_root, &BTreeSet::from([Capability::Status])) {
        Ok(mut client) => {
            let helper_version = Some(client.helper_version.clone());
            let capabilities = client
                .capabilities
                .iter()
                .map(|capability| capability.wire_name().to_owned())
                .collect();
            let access = client.public_access_status();
            match client.exchange(
                HostMessage::Status(StatusRequest {
                    requested_core_generation_id: Some(active_core_generation_id.to_owned()),
                }),
                HANDSHAKE_TIMEOUT,
            ) {
                Ok(HelperMessage::Status(result)) => {
                    let (ready, materialized, error_code) = status_outcome(
                        &result,
                        client.authorization_state,
                        active_core_generation_id,
                    );
                    let valid_status = status_authority_error(&result, active_core_generation_id)
                        .is_none()
                        .then_some(&result);
                    ProStatus {
                        schema_version: 1,
                        installed: true,
                        ready,
                        materialized,
                        helper_path,
                        helper_version,
                        protocol_version: PROTOCOL_VERSION,
                        capabilities,
                        error_code: error_code.map(ToOwned::to_owned),
                        projection_currentness: valid_status.map(|status| status.currentness),
                        materialized_coverage: valid_status.map(|status| status.coverage),
                        repository_coverage: valid_status.map(|status| status.repository_coverage),
                        supported_operations: valid_status
                            .map(|status| status.supported_operations.clone()),
                        available_operations: valid_status
                            .map(|status| status.available_operations.clone()),
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
                    projection_currentness: None,
                    materialized_coverage: None,
                    repository_coverage: None,
                    supported_operations: None,
                    available_operations: None,
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
                    projection_currentness: None,
                    materialized_coverage: None,
                    repository_coverage: None,
                    supported_operations: None,
                    available_operations: None,
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
            projection_currentness: None,
            materialized_coverage: None,
            repository_coverage: None,
            supported_operations: None,
            available_operations: None,
            access_state: None,
            refresh_after_unix: None,
            access_deadline_unix: None,
            grace_deadline_unix: None,
            setup_repairability: ProSetupRepairability::NotNeeded,
        },
    }
}

pub(super) fn status_outcome(
    status: &StatusResult,
    authorization_state: Option<EntitlementAccessState>,
    active_core_generation_id: &str,
) -> (bool, bool, Option<&'static str>) {
    if let Some(error) = status_authority_error(status, active_core_generation_id) {
        return (false, false, Some(error));
    }
    let materialized = status.currentness == CoreProjectionCurrentness::Current
        && matches!(
            status.coverage,
            MaterializedCoverage::Complete
                | MaterializedCoverage::Empty
                | MaterializedCoverage::Abstained
        );
    if authorization_state == Some(EntitlementAccessState::Locked) {
        return (false, materialized, Some("entitlement_expired"));
    }
    let error = match status.currentness {
        CoreProjectionCurrentness::NotMaterialized => Some("not_materialized"),
        CoreProjectionCurrentness::NeedsRebuild => Some("needs_rebuild"),
        CoreProjectionCurrentness::Partial => Some("partial"),
        CoreProjectionCurrentness::Stale => Some("stale_source"),
        CoreProjectionCurrentness::Current => None,
    };
    (!status.available_operations.is_empty(), materialized, error)
}

fn status_authority_error(
    status: &StatusResult,
    active_core_generation_id: &str,
) -> Option<&'static str> {
    if status.requested_core_generation_id.as_deref() != Some(active_core_generation_id) {
        return Some("protocol_mismatch");
    }
    if status
        .core_receipt
        .as_ref()
        .is_some_and(|receipt| receipt.validate().is_err())
    {
        return Some("protocol_mismatch");
    }
    if status.currentness == CoreProjectionCurrentness::Current
        && status
            .core_receipt
            .as_ref()
            .is_some_and(|receipt| receipt.core_generation_id != active_core_generation_id)
    {
        return Some("stale_source");
    }
    status.validate().is_err().then_some("protocol_mismatch")
}
