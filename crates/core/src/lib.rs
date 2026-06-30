//! Provider agnostic domain types and traits that the rest of Shepherd builds on.

pub mod agent;
pub mod errors;
pub mod ids;
pub mod sandbox;
pub mod session;
pub mod workspace;

pub use agent::{AgentEvent, AgentRunner, RunRequest, RunResult};
pub use errors::{Error, Result};
pub use ids::{ProviderId, SandboxId, SessionId};
pub use sandbox::{
    ExecOptions, ExecResult, PtyControl, PtyOptions, PtySession, Sandbox, SandboxProvider,
    SandboxResources, SandboxSpec, SandboxStatus,
};
pub use session::{default_branch_for, Session, SessionStatus};
pub use workspace::{
    ArchiveWorkspaceSpec, DirtyOverlay, GitWorkspaceSpec, WorkspaceSpec, DEFAULT_MOUNT_PATH,
};
