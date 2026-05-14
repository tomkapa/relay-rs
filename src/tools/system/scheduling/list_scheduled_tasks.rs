//! `list_scheduled_tasks` — list the caller's active scheduled tasks.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use tracing::warn;

use crate::scheduling::{
    ScheduleSpec, ScheduledTaskId, ScheduledTaskName, ScheduledTaskRecord, SharedScheduledTaskStore,
};
use crate::tools::{Tool, ToolCallContext, ToolError};
use crate::types::ToolName;

const TOOL_NAME: &str = "list_scheduled_tasks";
const TOOL_DESCRIPTION: &str = "List your active scheduled tasks. Use this to recall \
    what's already scheduled before creating a new task or to answer the user's question \
    about what they have set up. Returns each task's id, name, schedule, and the next time \
    it will fire. Cancelled or completed tasks are not returned.";

#[derive(Debug, Serialize)]
struct Output {
    tasks: Vec<ListedTask>,
}

#[derive(Debug, Serialize)]
struct ListedTask {
    task_id: ScheduledTaskId,
    name: ScheduledTaskName,
    schedule: ScheduleSpec,
    next_run_at: Option<DateTime<Utc>>,
}

impl From<ScheduledTaskRecord> for ListedTask {
    fn from(r: ScheduledTaskRecord) -> Self {
        Self {
            task_id: r.id,
            name: r.name,
            schedule: r.schedule,
            next_run_at: r.next_run_at,
        }
    }
}

pub struct ListScheduledTasksTool {
    name: ToolName,
    description: &'static str,
    input_schema: Arc<Value>,
    store: SharedScheduledTaskStore,
}

impl std::fmt::Debug for ListScheduledTasksTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ListScheduledTasksTool")
            .finish_non_exhaustive()
    }
}

impl ListScheduledTasksTool {
    #[must_use]
    pub fn new(store: SharedScheduledTaskStore) -> Self {
        let name =
            ToolName::try_from(TOOL_NAME).expect("invariant: list_scheduled_tasks valid name");
        let input_schema = Arc::new(json!({
            "type": "object",
            "properties": {},
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
impl Tool for ListScheduledTasksTool {
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
        name = "tool.list_scheduled_tasks",
        fields(relay.from.viewer = %ctx.viewer),
    )]
    async fn execute(&self, _input: Value, ctx: &ToolCallContext) -> Result<String, ToolError> {
        let owner = ctx.viewer.agent_id().ok_or_else(|| {
            ToolError::InvalidInput("list_scheduled_tasks: caller must be an agent".into())
        })?;
        let rows = self.store.list_for_agent(owner).await.map_err(|e| {
            warn!(error = %e, relay.agent.id = %owner, "list_scheduled_tasks.failed");
            ToolError::Backend(format!("list_scheduled_tasks: {e}"))
        })?;
        let out = Output {
            tasks: rows.into_iter().map(ListedTask::from).collect(),
        };
        Ok(serde_json::to_string(&out)?)
    }
}
