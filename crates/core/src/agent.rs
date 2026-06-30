//! Drives a headless coding agent inside a sandbox. We do NOT reimplement the
//! agent loop; for Claude Code this shells out to `claude -p ... --resume` (see
//! PLAN.md section 6). Other CLIs are a different runner, not a rewrite.

use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::errors::Result;
use crate::ids::SandboxId;
use crate::sandbox::SandboxProvider;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    /// The agent's own conversation id, captured for later resume.
    Session { agent_session_id: String },
    Text { text: String },
    ToolUse { name: String, input: serde_json::Value },
    ToolResult { name: String, ok: bool },
    Error { message: String },
    Done { exit_code: i64 },
}

#[derive(Debug, Clone)]
pub struct RunRequest {
    pub sandbox_id: SandboxId,
    pub prompt: String,
    /// Working directory inside the box (the seeded workspace).
    pub cwd: String,
    /// Resume an existing agent conversation by id (Claude session_id). None
    /// starts a fresh conversation.
    pub resume_agent_session_id: Option<String>,
    /// Allowed tools, passed through to the agent CLI.
    pub allowed_tools: Vec<String>,
    pub env: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct RunResult {
    /// The agent's own session id, captured for later resume.
    pub agent_session_id: Option<String>,
    pub exit_code: i64,
}

#[async_trait]
pub trait AgentRunner: Send + Sync {
    /// Human readable runner name, e.g. "claude-code".
    fn name(&self) -> &str;

    /// Run one task non interactively, streaming events to `events`. Resolves
    /// when the agent process exits.
    async fn run(
        &self,
        provider: &dyn SandboxProvider,
        req: RunRequest,
        events: mpsc::Sender<AgentEvent>,
    ) -> Result<RunResult>;
}
