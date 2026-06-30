//! The provider agnostic contract for a cloud (or local) sandbox: a Linux box we
//! can boot, run commands in, attach an interactive terminal to, move files in
//! and out of, and snapshot/suspend for cheap idle persistence.
//!
//! Concrete adapters (docker, e2b, fly) implement [`SandboxProvider`].

use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

use crate::errors::{Error, Result};
use crate::ids::{ProviderId, SandboxId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SandboxStatus {
    Creating,
    Running,
    Suspended,
    Stopped,
    Error,
}

/// Resource request for a new sandbox.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SandboxResources {
    pub cpus: Option<f64>,
    pub memory_mb: Option<u64>,
    pub disk_mb: Option<u64>,
}

/// Everything needed to boot a box (but not yet seed a workspace).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SandboxSpec {
    /// Container/VM image with the agent runtime plus MCP runtimes baked in.
    pub image: String,
    pub resources: SandboxResources,
    /// Env injected at boot. Secret values come from the control plane, not files.
    pub env: HashMap<String, String>,
    /// Labels for bookkeeping (e.g. session id), surfaced by list().
    pub labels: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sandbox {
    pub id: SandboxId,
    pub provider_id: ProviderId,
    pub status: SandboxStatus,
    pub image: String,
    /// ISO-8601 creation timestamp.
    pub created_at: String,
    pub labels: HashMap<String, String>,
}

/// Options for a one shot command.
#[derive(Debug, Clone, Default)]
pub struct ExecOptions {
    pub cwd: Option<String>,
    pub env: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct ExecResult {
    pub exit_code: i64,
    pub stdout: String,
    pub stderr: String,
}

/// Control messages sent to an attached pty.
#[derive(Debug, Clone, Copy)]
pub enum PtyControl {
    Resize { cols: u16, rows: u16 },
    Kill,
}

/// Options for attaching an interactive terminal.
#[derive(Debug, Clone, Default)]
pub struct PtyOptions {
    pub cwd: Option<String>,
    pub env: HashMap<String, String>,
    pub cols: Option<u16>,
    pub rows: Option<u16>,
}

/// An attached, reattachable interactive terminal.
///
/// The provider wires the channels to the box: write to `input`, read terminal
/// bytes from `output`, send resize/kill via `control`, await process exit on
/// `exit`. Detaching the local viewport just drops the receiver; the box side is
/// owned by the control daemon (see PLAN.md section 8).
pub struct PtySession {
    pub input: mpsc::Sender<Vec<u8>>,
    pub output: mpsc::Receiver<Vec<u8>>,
    pub control: mpsc::Sender<PtyControl>,
    pub exit: oneshot::Receiver<i64>,
}

#[async_trait]
pub trait SandboxProvider: Send + Sync {
    /// Stable provider id, e.g. "docker".
    fn id(&self) -> ProviderId;

    /// Boot a new box. Does not seed a workspace; that is the agent layer's job.
    async fn create(&self, spec: SandboxSpec) -> Result<Sandbox>;

    /// Look up a box by id, or None if it no longer exists.
    async fn get(&self, id: &SandboxId) -> Result<Option<Sandbox>>;

    /// List boxes this provider manages, optionally filtered by labels.
    async fn list(&self, labels: &HashMap<String, String>) -> Result<Vec<Sandbox>>;

    /// Run a command to completion.
    async fn exec(&self, id: &SandboxId, command: &[String], opts: ExecOptions) -> Result<ExecResult>;

    /// Attach an interactive, reattachable terminal (e.g. `claude` itself).
    async fn attach_pty(&self, id: &SandboxId, command: &[String], opts: PtyOptions) -> Result<PtySession>;

    /// Write a single file into the box (seeding overlays, configs).
    async fn put_file(&self, id: &SandboxId, path: &str, content: &[u8], mode: u32) -> Result<()>;

    /// Read a single file out of the box (reconcile, inspection).
    async fn get_file(&self, id: &SandboxId, path: &str) -> Result<Vec<u8>>;

    /// Tear down a box and release its resources.
    async fn destroy(&self, id: &SandboxId) -> Result<()>;

    // Cheap idle persistence. Providers that cannot do these return NotSupported.

    async fn snapshot(&self, _id: &SandboxId) -> Result<String> {
        Err(self.not_supported("snapshot"))
    }

    async fn suspend(&self, _id: &SandboxId) -> Result<()> {
        Err(self.not_supported("suspend"))
    }

    async fn resume(&self, _id: &SandboxId) -> Result<()> {
        Err(self.not_supported("resume"))
    }

    fn not_supported(&self, op: &str) -> Error {
        Error::NotSupported {
            provider: self.id(),
            op: op.to_string(),
        }
    }
}
