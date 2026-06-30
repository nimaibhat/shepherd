//! A Shepherd session: one persistent agent working in one sandbox against one
//! seeded workspace. This is the unit a user launches, detaches from, and
//! reattaches to.

use serde::{Deserialize, Serialize};

use crate::ids::{ProviderId, SandboxId, SessionId};
use crate::workspace::WorkspaceSpec;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStatus {
    /// Created, no sandbox yet.
    Pending,
    /// Sandbox booting plus workspace seeding.
    Seeding,
    /// Agent actively working.
    Running,
    /// Agent waiting; box may be suspended to save cost.
    Idle,
    /// Box suspended to disk.
    Suspended,
    Error,
    Done,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub title: String,
    pub status: SessionStatus,
    pub provider_id: ProviderId,
    pub sandbox_id: Option<SandboxId>,
    pub workspace: WorkspaceSpec,
    /// The agent's own conversation id (Claude session_id), for resume.
    pub agent_session_id: Option<String>,
    /// Branch the agent commits to for durable output plus reconcile.
    pub branch: String,
    /// ISO-8601 timestamps.
    pub created_at: String,
    pub updated_at: String,
    /// Last error message, when status is Error.
    pub error: Option<String>,
}

/// The conventional branch an agent commits to for a session.
pub fn default_branch_for(id: &SessionId) -> String {
    format!("agent/{id}")
}
