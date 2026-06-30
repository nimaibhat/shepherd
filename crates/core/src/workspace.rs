//! How a sandbox's workspace is seeded.
//!
//! Git is the transport, not a network mount. The canonical workspace lives in
//! the sandbox after seeding; we never live mirror the user's local filesystem
//! (see PLAN.md section 2). Seeding is a one time, point in time operation.

use serde::{Deserialize, Serialize};

pub const DEFAULT_MOUNT_PATH: &str = "/workspace";

/// A captured snapshot of uncommitted local work, applied on top of a clone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirtyOverlay {
    /// Output of `git diff HEAD` (tracked changes), empty if none.
    pub diff: String,
    /// Untracked but not ignored files, as a gzip tar. Applied after the diff so
    /// the agent starts from exactly what the user sees locally.
    pub untracked_tar_gz: Option<Vec<u8>>,
    /// The commit the diff/overlay was captured against, for sanity checks.
    pub base_commit: String,
}

/// Seed from a git remote, the primary preferred path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitWorkspaceSpec {
    pub repo_url: String,
    /// Branch, tag, or commit to check out. None means the remote HEAD.
    pub reference: Option<String>,
    /// Shallow clone depth. None for a full clone.
    pub depth: Option<u32>,
    /// Optional uncommitted local state to overlay on top of the checkout.
    pub dirty_overlay: Option<DirtyOverlay>,
    /// Where to place the checkout inside the sandbox. None means /workspace.
    pub mount_path: Option<String>,
}

/// Seed from an uploaded archive, for non git folders (degenerate case).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveWorkspaceSpec {
    /// gzip tar of the directory tree.
    pub tar_gz: Vec<u8>,
    pub mount_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WorkspaceSpec {
    Git(GitWorkspaceSpec),
    Archive(ArchiveWorkspaceSpec),
}

impl WorkspaceSpec {
    /// The path the workspace is mounted at inside the sandbox.
    pub fn mount_path(&self) -> &str {
        let configured = match self {
            WorkspaceSpec::Git(g) => g.mount_path.as_deref(),
            WorkspaceSpec::Archive(a) => a.mount_path.as_deref(),
        };
        configured.unwrap_or(DEFAULT_MOUNT_PATH)
    }
}
