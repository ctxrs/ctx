use ctx_route_contracts::tasks::{
    CreateTaskRouteRequest, CreateTaskRouteSpec, CreateTaskSessionRouteRequest,
    CreateTaskSessionRouteSpec, ListWorkspaceTasksRouteParams,
};

use crate::daemon::{TaskCreationHandle, TaskSessionAdmissionHandle};

use super::super::{CreateTaskInput, CreateTaskSessionInput};
use super::common::{
    task_route_error_from_task_create, task_route_error_from_task_session_create, TaskRouteError,
    TaskRouteParams,
};
use super::responses::{SessionRouteResponse, TaskRouteResponse};

impl TaskCreationHandle {
    pub async fn create_task_for_route(
        &self,
        raw_workspace_id: &str,
        req: CreateTaskRouteRequest,
    ) -> Result<TaskRouteResponse, TaskRouteError> {
        let workspace_id =
            ListWorkspaceTasksRouteParams::new(raw_workspace_id).parse_workspace_id()?;
        let input = create_task_input_from_route_spec(req.into_spec()?);
        self.create_task_for_workspace(workspace_id, input)
            .await
            .map(TaskRouteResponse::from)
            .map_err(task_route_error_from_task_create)
    }
}

impl TaskSessionAdmissionHandle {
    pub async fn create_session_for_task_route(
        &self,
        params: TaskRouteParams,
        req: CreateTaskSessionRouteRequest,
        run_id_header: Option<String>,
    ) -> Result<SessionRouteResponse, TaskRouteError> {
        let task_id = params.parse_task_id()?;
        self.create_session_for_task(
            task_id,
            task_session_input_from_route_spec(req.into_spec(run_id_header)),
        )
        .await
        .map(SessionRouteResponse::from)
        .map_err(task_route_error_from_task_session_create)
    }
}

fn create_task_input_from_route_spec(spec: CreateTaskRouteSpec) -> CreateTaskInput {
    CreateTaskInput {
        task_id: spec.task_id,
        title: spec.title,
        description: spec.description,
        default_session: spec.default_session.map(task_session_input_from_route_spec),
    }
}

fn task_session_input_from_route_spec(spec: CreateTaskSessionRouteSpec) -> CreateTaskSessionInput {
    CreateTaskSessionInput {
        id: spec.id,
        provider_id: spec.provider_id,
        model_id: spec.model_id,
        reasoning_effort: spec.reasoning_effort,
        remember_model_preference: spec.remember_model_preference,
        parent_session_id: spec.parent_session_id,
        relationship: spec.relationship,
        initial_prompt: spec.initial_prompt,
        initial_message_id: spec.initial_message_id,
        initial_turn_id: spec.initial_turn_id,
        worktree_id: spec.worktree_id,
        execution_environment: spec.execution_environment,
        run_id_header: spec.run_id_header,
    }
}
