mod destination;
mod git;
mod path_policy;
mod staging;
mod status;
mod workflow;
mod workspace_init;
mod workspace_registration;

pub use destination::RepoValidateDestinationRequest;
pub use status::RepoStatusCheck;
pub use workflow::{
    clone_repo_with_service_errors, create_repo_staging_path_with_service_errors,
    initialize_repo_with_service_errors, inspect_repo_status_with_service_errors,
    validate_repo_destination_with_service_errors, RepoCloneRequest, RepoInitRequest,
    RepoOnboardingServiceError, RepoOnboardingServiceErrorKind,
};
pub use workspace_init::{init_workspace, init_workspace_at};
pub use workspace_registration::{
    prepare_workspace_registration, validate_workspace_primary_branch,
    validate_workspace_root_repo, WorkspaceRegistrationCandidate, WorkspaceRegistrationError,
};
