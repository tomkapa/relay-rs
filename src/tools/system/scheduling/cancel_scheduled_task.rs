//! `cancel_scheduled_task` — cancel one of the caller's scheduled tasks.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::scheduling::{ScheduledTaskError, ScheduledTaskId, SharedScheduledTaskStore};
use crate::tools::{Tool, ToolCallContext, ToolError};
use crate::types::ToolName;

const TOOL_NAME: &str = "cancel_scheduled_task";
const TOOL_DESCRIPTION: &str = "Cancel one of your scheduled tasks. You can only \
    cancel tasks you yourself scheduled. Cancelling an already-cancelled or completed task \
    is a no-op.\n\
    \n\
    Argument: `task_id` from `list_scheduled_tasks` (or the id you remembered when you \
    created the task).";

#[derive(Debug, Deserialize)]
struct Input {
    task_id: ScheduledTaskId,
}

#[derive(Debug, Serialize)]
struct Output {
    task_id: ScheduledTaskId,
}

pub struct CancelScheduledTaskTool {
    name: ToolName,
    description: &'static str,
    input_schema: Arc<Value>,
    store: SharedScheduledTaskStore,
}

impl std::fmt::Debug for CancelScheduledTaskTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancelScheduledTaskTool")
            .finish_non_exhaustive()
    }
}

impl CancelScheduledTaskTool {
    #[must_use]
    pub fn new(store: SharedScheduledTaskStore) -> Self {
        let name =
            ToolName::try_from(TOOL_NAME).expect("invariant: cancel_scheduled_task valid name");
        let input_schema = Arc::new(json!({
            "type": "object",
            "required": ["task_id"],
            "properties": {
                "task_id": { "type": "string", "format": "uuid" }
            },
            "additionalProperties": false
        }));
        Self {
            name,
            description: TOOL_DESCRIPTION,
            input_schema,
            store,
        }
    }
}

#[async_trait]
impl Tool for CancelScheduledTaskTool {
    fn name(&self) -> &ToolName {
        &self.name
    }
    fn description(&self) -> &str {
        self.description
    }
    fn input_schema(&self) -> Arc<Value> {
        self.input_schema.clone()
    }

    #[tracing::instrument(
        skip_all,
        name = "tool.cancel_scheduled_task",
        fields(relay.from.viewer = %ctx.viewer),
    )]
    async fn execute(&self, input: Value, ctx: &ToolCallContext) -> Result<String, ToolError> {
        let parsed: Input = serde_json::from_value(input)?;
        let owner = ctx.viewer.agent_id().ok_or_else(|| {
            ToolError::InvalidInput("cancel_scheduled_task: caller must be an agent".into())
        })?;
        match self.store.cancel(parsed.task_id, owner).await {
            Ok(()) => {
                info!(
                    relay.scheduled_task.id = %parsed.task_id,
                    relay.agent.id = %owner,
                    "cancel_scheduled_task.cancelled",
                );
                Ok(serde_json::to_string(&Output {
                    task_id: parsed.task_id,
                })?)
            }
            // `cancel` folds the cross-owner case into `NotFound` so the
            // tool seam cannot be used to probe for other agents' rows.
            Err(ScheduledTaskError::NotFound(_)) => Err(ToolError::InvalidInput(format!(
                "cancel_scheduled_task: task {} not found",
                parsed.task_id
            ))),
            Err(e) => {
                warn!(error = %e, relay.scheduled_task.id = %parsed.task_id,
                    "cancel_scheduled_task.failed");
                Err(ToolError::Backend(format!("cancel_scheduled_task: {e}")))
            }
        }
    }
}
