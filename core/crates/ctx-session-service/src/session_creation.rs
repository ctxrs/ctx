use ctx_core::ids::{SessionId, TaskId, WorkspaceId, WorktreeId};
use ctx_core::models::{ExecutionEnvironment, Session};
use ctx_session_tools::model_resolution::compose_model_id;

#[derive(Debug, Clone, Copy)]
pub struct CreateSessionRequestPolicy<'a> {
    pub requested_session_id: Option<&'a str>,
    pub parent_session_id: Option<&'a str>,
    pub relationship: Option<&'a str>,
    pub initial_prompt_present: bool,
    pub initial_message_id_present: bool,
    pub initial_turn_id_present: bool,
    pub task_primary_session_id: Option<SessionId>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CreateSessionRequestDecision {
    pub session_id: Option<SessionId>,
    pub parent_session_id: Option<SessionId>,
    pub relationship: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct LoadedSessionRequestInput<'a> {
    pub run_id_header: Option<&'a str>,
    pub provider_id: &'a str,
    pub provider_can_create_loaded_session: bool,
    pub requested_session_id: Option<&'a str>,
    pub parent_session_id: Option<&'a str>,
    pub relationship: Option<&'a str>,
    pub initial_prompt_present: bool,
    pub initial_message_id_present: bool,
    pub initial_turn_id_present: bool,
    pub task_primary_session_id: Option<SessionId>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct LoadedSessionRequestDecision {
    pub run_id_header: Option<String>,
    pub provider_id: String,
    pub session_id: Option<SessionId>,
    pub parent_session_id: Option<SessionId>,
    pub relationship: Option<String>,
    pub requested_relationship: Option<String>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum LoadedSessionRequestError {
    ProviderUnavailable,
    InvalidSessionId,
    InvalidParentSessionId,
    RelationshipRequiresParent,
    MissingInitialPromptIds,
    PrimarySessionConflict,
}

impl LoadedSessionRequestError {
    pub fn compat_issue(self) -> Option<&'static str> {
        match self {
            Self::MissingInitialPromptIds => Some("missing_initial_ids"),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum CreateSessionRequestError {
    InvalidSessionId,
    InvalidParentSessionId,
    RelationshipRequiresParent,
    MissingInitialPromptIds,
    PrimarySessionConflict,
}

#[derive(Debug, Clone, Copy)]
pub struct SessionCreationIdentity<'a> {
    pub task_id: TaskId,
    pub workspace_id: WorkspaceId,
    pub worktree_id: WorktreeId,
    pub execution_environment: ExecutionEnvironment,
    pub provider_id: &'a str,
    pub model_id: &'a str,
    pub reasoning_effort: Option<&'a str>,
    pub parent_session_id: Option<SessionId>,
    pub relationship: Option<&'a str>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DefaultSessionSeed {
    pub provider_id: String,
    pub model_id: String,
    pub reasoning_effort: Option<String>,
    pub execution_environment: ExecutionEnvironment,
}

pub fn validate_create_session_request(
    input: CreateSessionRequestPolicy<'_>,
) -> Result<CreateSessionRequestDecision, CreateSessionRequestError> {
    let session_id = parse_optional_session_id(input.requested_session_id)
        .map_err(|_| CreateSessionRequestError::InvalidSessionId)?;
    let parent_session_id = parse_optional_session_id(input.parent_session_id)
        .map_err(|_| CreateSessionRequestError::InvalidParentSessionId)?;
    let relationship = input
        .relationship
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    if parent_session_id.is_some() != relationship.is_some() {
        return Err(CreateSessionRequestError::RelationshipRequiresParent);
    }
    if parent_session_id.is_none() && relationship.is_none() {
        if let Some(primary_session_id) = input.task_primary_session_id {
            if session_id.as_ref() != Some(&primary_session_id) {
                return Err(CreateSessionRequestError::PrimarySessionConflict);
            }
        }
    }
    if input.initial_prompt_present
        && (!input.initial_message_id_present || !input.initial_turn_id_present)
    {
        return Err(CreateSessionRequestError::MissingInitialPromptIds);
    }

    Ok(CreateSessionRequestDecision {
        session_id,
        parent_session_id,
        relationship,
    })
}

pub fn prepare_loaded_session_request(
    input: LoadedSessionRequestInput<'_>,
) -> Result<LoadedSessionRequestDecision, LoadedSessionRequestError> {
    let provider_id = input.provider_id.trim().to_string();
    if !input.provider_can_create_loaded_session {
        return Err(LoadedSessionRequestError::ProviderUnavailable);
    }
    let session_request = validate_create_session_request(CreateSessionRequestPolicy {
        requested_session_id: input.requested_session_id,
        parent_session_id: input.parent_session_id,
        relationship: input.relationship,
        initial_prompt_present: input.initial_prompt_present,
        initial_message_id_present: input.initial_message_id_present,
        initial_turn_id_present: input.initial_turn_id_present,
        task_primary_session_id: input.task_primary_session_id,
    })
    .map_err(loaded_session_request_error)?;
    let requested_relationship = session_request.relationship.clone();
    Ok(LoadedSessionRequestDecision {
        run_id_header: input.run_id_header.map(str::to_string),
        provider_id,
        session_id: session_request.session_id,
        parent_session_id: session_request.parent_session_id,
        relationship: session_request.relationship,
        requested_relationship,
    })
}

pub fn should_preflight_default_session(
    existing_task_present: bool,
    explicit_default_session_present: bool,
) -> bool {
    !existing_task_present && !explicit_default_session_present
}

pub fn default_session_id_for_existing_primary(
    requested_session_id: Option<&str>,
    primary_session_id: SessionId,
) -> String {
    match requested_session_id {
        Some(raw) if !raw.trim().is_empty() => raw.to_string(),
        _ => primary_session_id.0.to_string(),
    }
}

pub fn compose_loaded_session_preferred_model_id(
    model_id: &str,
    reasoning_effort: Option<&str>,
) -> String {
    compose_model_id(model_id, reasoning_effort)
}

pub fn session_matches_creation_identity(
    session: &Session,
    expected: SessionCreationIdentity<'_>,
) -> bool {
    session.task_id == expected.task_id
        && session.workspace_id == expected.workspace_id
        && session.worktree_id == expected.worktree_id
        && session.execution_environment == expected.execution_environment
        && session.provider_id == expected.provider_id
        && session.model_id == expected.model_id
        && session.reasoning_effort.as_deref() == expected.reasoning_effort
        && session.parent_session_id == expected.parent_session_id
        && session.relationship.as_deref() == expected.relationship
}

fn parse_optional_session_id(raw: Option<&str>) -> Result<Option<SessionId>, uuid::Error> {
    match raw.map(str::trim) {
        Some("") | None => Ok(None),
        Some(value) => uuid::Uuid::parse_str(value).map(SessionId).map(Some),
    }
}

fn loaded_session_request_error(error: CreateSessionRequestError) -> LoadedSessionRequestError {
    match error {
        CreateSessionRequestError::InvalidSessionId => LoadedSessionRequestError::InvalidSessionId,
        CreateSessionRequestError::InvalidParentSessionId => {
            LoadedSessionRequestError::InvalidParentSessionId
        }
        CreateSessionRequestError::RelationshipRequiresParent => {
            LoadedSessionRequestError::RelationshipRequiresParent
        }
        CreateSessionRequestError::MissingInitialPromptIds => {
            LoadedSessionRequestError::MissingInitialPromptIds
        }
        CreateSessionRequestError::PrimarySessionConflict => {
            LoadedSessionRequestError::PrimarySessionConflict
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use ctx_core::models::SessionStatus;

    #[test]
    fn parses_requested_session_and_parent_relationship() {
        let session_id = SessionId::new();
        let parent_id = SessionId::new();

        let decision = validate_create_session_request(CreateSessionRequestPolicy {
            requested_session_id: Some(&session_id.0.to_string()),
            parent_session_id: Some(&parent_id.0.to_string()),
            relationship: Some(" branch "),
            initial_prompt_present: false,
            initial_message_id_present: false,
            initial_turn_id_present: false,
            task_primary_session_id: None,
        })
        .expect("valid request");

        assert_eq!(decision.session_id, Some(session_id));
        assert_eq!(decision.parent_session_id, Some(parent_id));
        assert_eq!(decision.relationship.as_deref(), Some("branch"));
    }

    #[test]
    fn rejects_relationship_without_parent_or_parent_without_relationship() {
        assert_eq!(
            validate_create_session_request(CreateSessionRequestPolicy {
                requested_session_id: None,
                parent_session_id: Some(&SessionId::new().0.to_string()),
                relationship: None,
                initial_prompt_present: false,
                initial_message_id_present: false,
                initial_turn_id_present: false,
                task_primary_session_id: None,
            }),
            Err(CreateSessionRequestError::RelationshipRequiresParent)
        );
        assert_eq!(
            validate_create_session_request(CreateSessionRequestPolicy {
                requested_session_id: None,
                parent_session_id: None,
                relationship: Some("branch"),
                initial_prompt_present: false,
                initial_message_id_present: false,
                initial_turn_id_present: false,
                task_primary_session_id: None,
            }),
            Err(CreateSessionRequestError::RelationshipRequiresParent)
        );
    }

    #[test]
    fn rejects_missing_initial_prompt_ids() {
        assert_eq!(
            validate_create_session_request(CreateSessionRequestPolicy {
                requested_session_id: None,
                parent_session_id: None,
                relationship: None,
                initial_prompt_present: true,
                initial_message_id_present: true,
                initial_turn_id_present: false,
                task_primary_session_id: None,
            }),
            Err(CreateSessionRequestError::MissingInitialPromptIds)
        );
    }

    #[test]
    fn rejects_conflicting_primary_session_creation() {
        let primary_id = SessionId::new();
        assert_eq!(
            validate_create_session_request(CreateSessionRequestPolicy {
                requested_session_id: None,
                parent_session_id: None,
                relationship: None,
                initial_prompt_present: false,
                initial_message_id_present: false,
                initial_turn_id_present: false,
                task_primary_session_id: Some(primary_id),
            }),
            Err(CreateSessionRequestError::PrimarySessionConflict)
        );
    }

    #[test]
    fn session_identity_requires_exact_creation_tuple_match() {
        let session = test_session();
        let matching = SessionCreationIdentity {
            task_id: session.task_id,
            workspace_id: session.workspace_id,
            worktree_id: session.worktree_id,
            execution_environment: session.execution_environment,
            provider_id: &session.provider_id,
            model_id: &session.model_id,
            reasoning_effort: session.reasoning_effort.as_deref(),
            parent_session_id: session.parent_session_id,
            relationship: session.relationship.as_deref(),
        };
        assert!(session_matches_creation_identity(&session, matching));

        let mismatched_model = SessionCreationIdentity {
            model_id: "different-model",
            ..matching
        };
        assert!(!session_matches_creation_identity(
            &session,
            mismatched_model
        ));
    }

    #[test]
    fn loaded_session_request_trims_provider_and_preserves_run_header() {
        let session_id = SessionId::new();
        let parent_id = SessionId::new();

        let decision = prepare_loaded_session_request(LoadedSessionRequestInput {
            run_id_header: Some("run-123"),
            provider_id: " fake ",
            provider_can_create_loaded_session: true,
            requested_session_id: Some(&session_id.0.to_string()),
            parent_session_id: Some(&parent_id.0.to_string()),
            relationship: Some(" branch "),
            initial_prompt_present: false,
            initial_message_id_present: false,
            initial_turn_id_present: false,
            task_primary_session_id: None,
        })
        .expect("valid loaded session request");

        assert_eq!(decision.run_id_header.as_deref(), Some("run-123"));
        assert_eq!(decision.provider_id, "fake");
        assert_eq!(decision.session_id, Some(session_id));
        assert_eq!(decision.parent_session_id, Some(parent_id));
        assert_eq!(decision.relationship.as_deref(), Some("branch"));
        assert_eq!(decision.requested_relationship.as_deref(), Some("branch"));
    }

    #[test]
    fn loaded_session_request_uses_daemon_provider_eligibility_boolean() {
        assert_eq!(
            prepare_loaded_session_request(LoadedSessionRequestInput {
                run_id_header: None,
                provider_id: "fake",
                provider_can_create_loaded_session: false,
                requested_session_id: None,
                parent_session_id: None,
                relationship: None,
                initial_prompt_present: false,
                initial_message_id_present: false,
                initial_turn_id_present: false,
                task_primary_session_id: None,
            }),
            Err(LoadedSessionRequestError::ProviderUnavailable)
        );
    }

    #[test]
    fn loaded_session_request_classifies_missing_initial_ids_for_daemon_counter() {
        let error = prepare_loaded_session_request(LoadedSessionRequestInput {
            run_id_header: None,
            provider_id: "fake",
            provider_can_create_loaded_session: true,
            requested_session_id: None,
            parent_session_id: None,
            relationship: None,
            initial_prompt_present: true,
            initial_message_id_present: true,
            initial_turn_id_present: false,
            task_primary_session_id: None,
        })
        .unwrap_err();

        assert_eq!(error, LoadedSessionRequestError::MissingInitialPromptIds);
        assert_eq!(error.compat_issue(), Some("missing_initial_ids"));
    }

    #[test]
    fn default_session_helpers_preserve_existing_primary_id_policy() {
        let primary_session_id = SessionId::new();

        assert!(should_preflight_default_session(false, false));
        assert!(!should_preflight_default_session(true, false));
        assert!(!should_preflight_default_session(false, true));
        assert_eq!(
            default_session_id_for_existing_primary(None, primary_session_id),
            primary_session_id.0.to_string()
        );
        assert_eq!(
            default_session_id_for_existing_primary(Some("   "), primary_session_id),
            primary_session_id.0.to_string()
        );
        assert_eq!(
            default_session_id_for_existing_primary(Some("explicit"), primary_session_id),
            "explicit"
        );
    }

    #[test]
    fn loaded_session_preferred_model_composition_preserves_reasoning_effort() {
        assert_eq!(
            compose_loaded_session_preferred_model_id("gpt-5.4", Some("high")),
            "gpt-5.4/high"
        );
        assert_eq!(
            compose_loaded_session_preferred_model_id("gpt-5.4", None),
            "gpt-5.4"
        );
    }

    fn test_session() -> Session {
        let now = Utc::now();
        Session {
            id: SessionId::new(),
            task_id: TaskId::new(),
            workspace_id: WorkspaceId::new(),
            worktree_id: WorktreeId::new(),
            execution_environment: ExecutionEnvironment::Host,
            parent_session_id: None,
            relationship: None,
            provider_id: "fake".to_string(),
            model_id: "fake-model".to_string(),
            reasoning_effort: Some("medium".to_string()),
            title: String::new(),
            agent_role: "assistant".to_string(),
            status: SessionStatus::Active,
            provider_session_ref: None,
            created_at: now,
            updated_at: now,
        }
    }
}
