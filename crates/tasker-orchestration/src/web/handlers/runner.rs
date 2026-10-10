//! Narrow runner-control surfaces used by external orchestration control planes.
//!
//! These endpoints deliberately expose Tasker's own validated registry and durable
//! transition history rather than requiring callers to couple to Tasker tables.

use std::collections::{BTreeMap, HashMap};

use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use tasker_shared::models::core::named_step::NamedStep;
use tasker_shared::models::core::named_task::NamedTask;
use tasker_shared::models::core::task::Task;
use tasker_shared::models::core::task_namespace::TaskNamespace;
use tasker_shared::models::core::task_template::TaskTemplate;
use tasker_shared::models::core::task_transition::TaskTransition;
use tasker_shared::models::core::workflow_step::WorkflowStep;
use tasker_shared::models::core::workflow_step_transition::WorkflowStepTransition;
use tasker_shared::registry::TaskTemplateRegistry;
use tasker_shared::types::api::orchestration::TaskResponse;
use tasker_shared::types::web::ApiError;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::web::state::AppState;

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ImmutableTemplateRegistrationRequest {
    /// Full TaskTemplate JSON. Kept as JSON at the HTTP boundary so this additive
    /// endpoint does not duplicate TaskTemplate's evolving OpenAPI schema.
    #[schema(value_type = Object)]
    pub template: serde_json::Value,
    /// Agent/control-plane content fingerprint bound into the template identity.
    pub content_hash: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ImmutableTemplateRegistrationResponse {
    pub namespace: String,
    pub name: String,
    pub version: String,
    pub content_hash: String,
    pub created: bool,
}

#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct ResolveTaskIdentityQuery {
    pub namespace: String,
    pub name: String,
    pub version: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct ExecutionEventsQuery {
    pub cursor: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ReplayCursor {
    task: i32,
    steps: BTreeMap<String, i32>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ExecutionTransitionEvent {
    pub cursor: String,
    pub kind: String,
    pub transition_uuid: String,
    pub task_uuid: String,
    pub workflow_step_uuid: Option<String>,
    pub step_name: Option<String>,
    pub from_state: Option<String>,
    pub to_state: String,
    pub sort_key: i32,
    pub occurred_at: String,
    #[schema(value_type = Option<Object>)]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ExecutionEventsPage {
    pub task_uuid: String,
    pub events: Vec<ExecutionTransitionEvent>,
    pub next_cursor: String,
    pub has_more: bool,
}

fn validate_content_hash(value: &str) -> Result<(), ApiError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ApiError::bad_request("content_hash must be a 64-character hex digest"));
    }
    Ok(())
}

fn validate_template_storage_bounds(template: &TaskTemplate) -> Result<(), ApiError> {
    // These bounds mirror Tasker's persisted schema. Validate before registry
    // mutation so an invalid external template cannot create a namespace and
    // then fail later as an opaque database error.
    if template.namespace_name.chars().count() > 64 {
        return Err(ApiError::bad_request("template namespace exceeds Tasker storage limit (64 characters)"));
    }
    if template.name.chars().count() > 64 {
        return Err(ApiError::bad_request("template name exceeds Tasker storage limit (64 characters)"));
    }
    if template.version.chars().count() > 16 {
        return Err(ApiError::bad_request("template version exceeds Tasker storage limit (16 characters)"));
    }
    if template.description.as_ref().is_some_and(|value| value.chars().count() > 255) {
        return Err(ApiError::bad_request("template description exceeds Tasker storage limit (255 characters)"));
    }
    for step in &template.steps {
        if step.name.chars().count() > 128 {
            return Err(ApiError::bad_request(format!(
                "step '{}' exceeds Tasker name storage limit (128 characters)",
                step.name
            )));
        }
        if step.description.as_ref().is_some_and(|value| value.chars().count() > 255) {
            return Err(ApiError::bad_request(format!(
                "step '{}' description exceeds Tasker storage limit (255 characters)",
                step.name
            )));
        }
    }
    Ok(())
}

fn template_declares_hash(template: &TaskTemplate, hash: &str) -> bool {
    template
        .metadata
        .as_ref()
        .map(|meta| meta.tags.iter().any(|tag| tag == &format!("sha256:{hash}")))
        .unwrap_or(false)
}

fn parse_cursor(value: Option<&str>) -> Result<ReplayCursor, ApiError> {
    let Some(value) = value else { return Ok(ReplayCursor::default()); };
    let payload = value
        .strip_prefix("v1:")
        .ok_or_else(|| ApiError::bad_request("unsupported execution-event cursor version"))?;
    serde_json::from_str(payload).map_err(|_| ApiError::bad_request("invalid execution-event cursor"))
}

fn encode_cursor(cursor: &ReplayCursor) -> String {
    format!("v1:{}", serde_json::to_string(cursor).expect("replay cursor serializes"))
}

fn iso(value: NaiveDateTime) -> String {
    value.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string()
}

#[utoipa::path(
    post,
    path = "/v1/templates/immutable",
    request_body = ImmutableTemplateRegistrationRequest,
    responses(
        (status = 200, description = "Existing identical immutable template", body = ImmutableTemplateRegistrationResponse),
        (status = 201, description = "Immutable template created", body = ImmutableTemplateRegistrationResponse),
        (status = 400, description = "Invalid request", body = ApiError),
        (status = 409, description = "Template identity already exists with different content", body = ApiError)
    ),
    tag = "templates"
)]
pub async fn register_immutable_template(
    State(state): State<AppState>,
    Json(request): Json<ImmutableTemplateRegistrationRequest>,
) -> Result<(axum::http::StatusCode, Json<ImmutableTemplateRegistrationResponse>), ApiError> {
    validate_content_hash(&request.content_hash)?;
    let template: TaskTemplate = serde_json::from_value(request.template)
        .map_err(|error| ApiError::bad_request(format!("invalid TaskTemplate: {error}")))?;
    template
        .validate()
        .map_err(|error| ApiError::bad_request(format!("invalid TaskTemplate: {error}")))?;
    validate_template_storage_bounds(&template)?;
    if !template_declares_hash(&template, &request.content_hash) {
        return Err(ApiError::bad_request("template metadata must contain sha256:<content_hash> tag"));
    }

    let registry = TaskTemplateRegistry::with_system_context(state.orchestration_core().context.clone());
    let exists = registry
        .is_template_registered(&template.namespace_name, &template.name, &template.version)
        .await
        .map_err(|error| ApiError::internal_server_error(error.to_string()))?;

    let created = if exists {
        let existing = registry
            .get_task_template(&template.namespace_name, &template.name, &template.version)
            .await
            .map_err(|error| ApiError::internal_server_error(error.to_string()))?;
        if existing != template {
            return Err(ApiError::conflict(format!(
                "template {}/{}/{} already exists with different content",
                template.namespace_name, template.name, template.version
            )));
        }
        false
    } else {
        registry
            .register_task_template(&template)
            .await
            .map_err(|error| ApiError::internal_server_error(error.to_string()))?;
        true
    };

    let response = ImmutableTemplateRegistrationResponse {
        namespace: template.namespace_name,
        name: template.name,
        version: template.version,
        content_hash: request.content_hash,
        created,
    };
    Ok((
        if created { axum::http::StatusCode::CREATED } else { axum::http::StatusCode::OK },
        Json(response),
    ))
}

#[utoipa::path(
    get,
    path = "/v1/tasks/resolve",
    params(ResolveTaskIdentityQuery),
    responses(
        (status = 200, description = "Task resolved by exact idempotency identity", body = TaskResponse),
        (status = 404, description = "No matching task", body = ApiError)
    ),
    tag = "tasks"
)]
pub async fn resolve_task_identity(
    State(state): State<AppState>,
    Query(query): Query<ResolveTaskIdentityQuery>,
) -> Result<Json<TaskResponse>, ApiError> {
    if query.idempotency_key.trim().is_empty() {
        return Err(ApiError::bad_request("idempotency_key is required"));
    }
    let pool = state.orchestration_db_pool();
    let namespace = TaskNamespace::find_by_name(pool, &query.namespace)
        .await
        .map_err(|error| ApiError::database_error(error.to_string()))?
        .ok_or_else(|| ApiError::not_found("template namespace not found"))?;
    let named_task = NamedTask::find_by_name_version_namespace(
        pool,
        &query.name,
        &query.version,
        namespace.task_namespace_uuid,
    )
    .await
    .map_err(|error| ApiError::database_error(error.to_string()))?
    .ok_or_else(|| ApiError::not_found("template not found"))?;
    let identity_hash = Task::compute_identity_hash(
        named_task.identity_strategy,
        named_task.named_task_uuid,
        &None,
        Some(&query.idempotency_key),
    )
    .map_err(ApiError::bad_request)?;
    let task = Task::find_by_identity_hash(pool, &identity_hash)
        .await
        .map_err(|error| ApiError::database_error(error.to_string()))?
        .ok_or_else(|| ApiError::not_found("task identity not found"))?;
    let response = state
        .task_service()
        .get_task(task.task_uuid)
        .await
        .map_err(|error| ApiError::internal_server_error(error.to_string()))?;
    Ok(Json(response))
}

#[derive(Debug, Clone)]
enum PendingTransition {
    Task(TaskTransition),
    Step { transition: WorkflowStepTransition, step_name: String },
}

impl PendingTransition {
    fn occurred_at(&self) -> NaiveDateTime {
        match self {
            Self::Task(value) => value.created_at,
            Self::Step { transition, .. } => transition.created_at,
        }
    }

    fn stable_id(&self) -> String {
        match self {
            Self::Task(value) => format!("t:{}", value.task_transition_uuid),
            Self::Step { transition, .. } => format!("s:{}", transition.workflow_step_transition_uuid),
        }
    }
}

#[utoipa::path(
    get,
    path = "/v1/tasks/{uuid}/events",
    params(("uuid" = String, Path, description = "Task UUID"), ExecutionEventsQuery),
    responses(
        (status = 200, description = "Durable task and step transition replay", body = ExecutionEventsPage),
        (status = 404, description = "Task not found", body = ApiError)
    ),
    tag = "tasks"
)]
pub async fn get_execution_events(
    State(state): State<AppState>,
    Path(uuid): Path<String>,
    Query(query): Query<ExecutionEventsQuery>,
) -> Result<Json<ExecutionEventsPage>, ApiError> {
    let task_uuid = Uuid::parse_str(&uuid).map_err(|_| ApiError::invalid_uuid(&uuid))?;
    let pool = state.orchestration_db_pool();
    Task::find_by_id(pool, task_uuid)
        .await
        .map_err(|error| ApiError::database_error(error.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("task {task_uuid}")))?;

    let mut cursor = parse_cursor(query.cursor.as_deref())?;
    let limit = query.limit.unwrap_or(200).clamp(1, 500);
    let mut pending = Vec::new();

    for transition in TaskTransition::list_by_task(pool, task_uuid)
        .await
        .map_err(|error| ApiError::database_error(error.to_string()))?
    {
        if transition.sort_key > cursor.task {
            pending.push(PendingTransition::Task(transition));
        }
    }

    let steps = WorkflowStep::list_by_task(pool, task_uuid)
        .await
        .map_err(|error| ApiError::database_error(error.to_string()))?;
    let named_steps = NamedStep::find_by_uuids(
        pool,
        &steps.iter().map(|step| step.named_step_uuid).collect::<Vec<_>>(),
    )
    .await
    .map_err(|error| ApiError::database_error(error.to_string()))?;
    let names: HashMap<Uuid, String> = named_steps
        .into_iter()
        .map(|step| (step.named_step_uuid, step.name))
        .collect();

    for step in steps {
        let seen = cursor.steps.get(&step.workflow_step_uuid.to_string()).copied().unwrap_or(0);
        let name = names
            .get(&step.named_step_uuid)
            .cloned()
            .unwrap_or_else(|| step.workflow_step_uuid.to_string());
        for transition in WorkflowStepTransition::list_by_workflow_step(pool, step.workflow_step_uuid)
            .await
            .map_err(|error| ApiError::database_error(error.to_string()))?
        {
            if transition.sort_key > seen {
                pending.push(PendingTransition::Step { transition, step_name: name.clone() });
            }
        }
    }

    pending.sort_by(|left, right| {
        left.occurred_at()
            .cmp(&right.occurred_at())
            .then_with(|| left.stable_id().cmp(&right.stable_id()))
    });
    let has_more = pending.len() > limit;
    let mut events = Vec::new();
    for transition in pending.into_iter().take(limit) {
        let event = match transition {
            PendingTransition::Task(value) => {
                cursor.task = cursor.task.max(value.sort_key);
                ExecutionTransitionEvent {
                    cursor: encode_cursor(&cursor),
                    kind: "TASK".to_string(),
                    transition_uuid: value.task_transition_uuid.to_string(),
                    task_uuid: task_uuid.to_string(),
                    workflow_step_uuid: None,
                    step_name: None,
                    from_state: value.from_state,
                    to_state: value.to_state,
                    sort_key: value.sort_key,
                    occurred_at: iso(value.created_at),
                    metadata: value.metadata,
                }
            }
            PendingTransition::Step { transition: value, step_name } => {
                cursor
                    .steps
                    .insert(value.workflow_step_uuid.to_string(), value.sort_key);
                ExecutionTransitionEvent {
                    cursor: encode_cursor(&cursor),
                    kind: "STEP".to_string(),
                    transition_uuid: value.workflow_step_transition_uuid.to_string(),
                    task_uuid: task_uuid.to_string(),
                    workflow_step_uuid: Some(value.workflow_step_uuid.to_string()),
                    step_name: Some(step_name),
                    from_state: value.from_state,
                    to_state: value.to_state,
                    sort_key: value.sort_key,
                    occurred_at: iso(value.created_at),
                    metadata: value.metadata,
                }
            }
        };
        events.push(event);
    }
    Ok(Json(ExecutionEventsPage {
        task_uuid: task_uuid.to_string(),
        events,
        next_cursor: encode_cursor(&cursor),
        has_more,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_cursor_round_trips_task_and_step_frontiers() {
        let step_uuid = Uuid::now_v7().to_string();
        let mut cursor = ReplayCursor { task: 7, steps: BTreeMap::new() };
        cursor.steps.insert(step_uuid.clone(), 3);

        let encoded = encode_cursor(&cursor);
        let decoded = parse_cursor(Some(&encoded)).expect("encoded cursor should decode");

        assert_eq!(decoded.task, 7);
        assert_eq!(decoded.steps.get(&step_uuid), Some(&3));
    }

    #[test]
    fn replay_cursor_rejects_unknown_versions() {
        assert!(parse_cursor(Some("v2:{\"task\":0,\"steps\":{}}")).is_err());
    }

    #[test]
    fn replay_cursor_rejects_malformed_payloads() {
        assert!(parse_cursor(Some("v1:not-json")).is_err());
    }
}
