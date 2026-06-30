//! Seed a sandbox's workspace from a WorkspaceSpec (PLAN.md section 3). Git is
//! the transport: clone the repo, optionally overlay uncommitted local state,
//! then cut the per session branch. This runs commands inside the box via the
//! provider; it never live mirrors the local filesystem.

use shepherd_core::errors::{Error, Result};
use shepherd_core::ids::SandboxId;
use shepherd_core::sandbox::{ExecOptions, SandboxProvider};
use shepherd_core::workspace::{
    ArchiveWorkspaceSpec, GitWorkspaceSpec, WorkspaceSpec, DEFAULT_MOUNT_PATH,
};

const OVERLAY_DIFF: &str = ".shepherd-overlay.diff";
const UNTRACKED_TGZ: &str = ".shepherd-untracked.tgz";
const ARCHIVE_TGZ: &str = ".shepherd-archive.tgz";

/// Seed the workspace and return the mount path it was placed at.
pub async fn seed_workspace(
    provider: &dyn SandboxProvider,
    id: &SandboxId,
    spec: &WorkspaceSpec,
    branch: &str,
) -> Result<String> {
    match spec {
        WorkspaceSpec::Git(g) => seed_git(provider, id, g, branch).await,
        WorkspaceSpec::Archive(a) => seed_archive(provider, id, a, branch).await,
    }
}

async fn seed_git(
    provider: &dyn SandboxProvider,
    id: &SandboxId,
    spec: &GitWorkspaceSpec,
    branch: &str,
) -> Result<String> {
    let mount = spec.mount_path.as_deref().unwrap_or(DEFAULT_MOUNT_PATH).to_string();

    // Clone. If a specific ref is requested we clone full then checkout, to avoid
    // shallow-clone limits when the ref is an arbitrary commit.
    let mut clone = vec!["git".into(), "clone".into()];
    if spec.reference.is_none() {
        if let Some(depth) = spec.depth {
            clone.push("--depth".into());
            clone.push(depth.to_string());
        }
    }
    clone.push(spec.repo_url.clone());
    clone.push(mount.clone());
    run_checked(provider, id, &clone, None).await?;

    if let Some(reference) = &spec.reference {
        run_checked(provider, id, &git_c(&mount, &["checkout", reference]), None).await?;
    }

    configure_identity(provider, id, &mount).await?;

    // Apply uncommitted local state captured on the user's machine.
    if let Some(overlay) = &spec.dirty_overlay {
        if !overlay.diff.trim().is_empty() {
            let path = format!("{mount}/{OVERLAY_DIFF}");
            provider.put_file(id, &path, overlay.diff.as_bytes(), 0o644).await?;
            run_checked(provider, id, &["git".into(), "apply".into(), OVERLAY_DIFF.into()], Some(&mount)).await?;
            run_checked(provider, id, &["rm".into(), "-f".into(), OVERLAY_DIFF.into()], Some(&mount)).await?;
        }
        if let Some(tar) = &overlay.untracked_tar_gz {
            let path = format!("{mount}/{UNTRACKED_TGZ}");
            provider.put_file(id, &path, tar, 0o644).await?;
            run_checked(provider, id, &["tar".into(), "xzf".into(), UNTRACKED_TGZ.into()], Some(&mount)).await?;
            run_checked(provider, id, &["rm".into(), "-f".into(), UNTRACKED_TGZ.into()], Some(&mount)).await?;
        }
    }

    // Cut the per session branch the agent commits to.
    run_checked(provider, id, &git_c(&mount, &["checkout", "-B", branch]), None).await?;
    Ok(mount)
}

async fn seed_archive(
    provider: &dyn SandboxProvider,
    id: &SandboxId,
    spec: &ArchiveWorkspaceSpec,
    branch: &str,
) -> Result<String> {
    let mount = spec.mount_path.as_deref().unwrap_or(DEFAULT_MOUNT_PATH).to_string();
    run_checked(provider, id, &["mkdir".into(), "-p".into(), mount.clone()], None).await?;

    let path = format!("{mount}/{ARCHIVE_TGZ}");
    provider.put_file(id, &path, &spec.tar_gz, 0o644).await?;
    run_checked(provider, id, &["tar".into(), "xzf".into(), ARCHIVE_TGZ.into()], Some(&mount)).await?;
    run_checked(provider, id, &["rm".into(), "-f".into(), ARCHIVE_TGZ.into()], Some(&mount)).await?;

    // Make it a git repo so the agent has a durable branch to commit to.
    configure_identity(provider, id, &mount).await?;
    run_checked(provider, id, &git_c(&mount, &["init", "-q"]), None).await?;
    run_checked(provider, id, &git_c(&mount, &["add", "-A"]), None).await?;
    run_checked(provider, id, &git_c(&mount, &["commit", "-q", "-m", "shepherd: seed archive"]), None).await?;
    run_checked(provider, id, &git_c(&mount, &["checkout", "-B", branch]), None).await?;
    Ok(mount)
}

async fn configure_identity(provider: &dyn SandboxProvider, id: &SandboxId, mount: &str) -> Result<()> {
    run_checked(provider, id, &git_c(mount, &["config", "user.email", "agent@shepherd.local"]), None).await?;
    run_checked(provider, id, &git_c(mount, &["config", "user.name", "Shepherd Agent"]), None).await?;
    Ok(())
}

/// Build a `git -C <dir> <args...>` command.
fn git_c(dir: &str, args: &[&str]) -> Vec<String> {
    let mut v = vec!["git".to_string(), "-C".to_string(), dir.to_string()];
    v.extend(args.iter().map(|s| s.to_string()));
    v
}

/// Run a command and turn a non zero exit into an ExecFailed error.
async fn run_checked(
    provider: &dyn SandboxProvider,
    id: &SandboxId,
    command: &[String],
    cwd: Option<&str>,
) -> Result<String> {
    let res = provider
        .exec(
            id,
            command,
            ExecOptions {
                cwd: cwd.map(str::to_string),
                env: Default::default(),
            },
        )
        .await?;
    if res.exit_code != 0 {
        return Err(Error::ExecFailed {
            cmd: command.join(" "),
            exit_code: res.exit_code,
            stderr: res.stderr,
        });
    }
    Ok(res.stdout)
}
