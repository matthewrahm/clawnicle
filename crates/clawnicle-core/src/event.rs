use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub sequence: i64,
    pub workflow_id: String,
    pub created_at_ms: i64,
    pub payload: EventPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventPayload {
    WorkflowStarted {
        name: String,
        input_hash: String,
    },
    ToolCallStarted {
        step_id: String,
        name: String,
        input_hash: String,
        attempt: u32,
    },
    ToolCallCompleted {
        step_id: String,
        output: serde_json::Value,
        duration_ms: u64,
    },
    ToolCallFailed {
        step_id: String,
        error: String,
        duration_ms: u64,
    },
    LlmCallCompleted {
        step_id: String,
        model: String,
        prompt_hash: String,
        response: String,
        tokens_in: u32,
        tokens_out: u32,
    },
    StepCompleted {
        step_id: String,
        name: String,
        output: serde_json::Value,
    },
    WorkflowCompleted {
        output: serde_json::Value,
    },
    WorkflowFailed {
        error: String,
    },
}

impl EventPayload {
    pub fn kind(&self) -> &'static str {
        match self {
            EventPayload::WorkflowStarted { .. } => "workflow_started",
            EventPayload::ToolCallStarted { .. } => "tool_call_started",
            EventPayload::ToolCallCompleted { .. } => "tool_call_completed",
            EventPayload::ToolCallFailed { .. } => "tool_call_failed",
            EventPayload::LlmCallCompleted { .. } => "llm_call_completed",
            EventPayload::StepCompleted { .. } => "step_completed",
            EventPayload::WorkflowCompleted { .. } => "workflow_completed",
            EventPayload::WorkflowFailed { .. } => "workflow_failed",
        }
    }
}
