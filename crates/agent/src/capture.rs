//! Capture a local git working directory into a WorkspaceSpec on the user's
//! machine (PLAN.md section 3a). This is the one place we read the local
//! filesystem: a one time, point in time snapshot of the repo URL plus any
//! uncommitted changes, so the sandbox starts from exactly what you see.

use std::path::Path;
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};

use shepherd_core::workspace::{DirtyOverlay, GitWorkspaceSpec};

/// Build a GitWorkspaceSpec from a local git repo, including a dirty overlay of
/// uncommitted tracked changes and untracked (non ignored) files.
pub fn capture_local_workspace(dir: &Path) -> Result<GitWorkspaceSpec> {
    let repo_url = git(dir, &["config", "--get", "remote.origin.url"])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            anyhow!("no git remote 'origin' found in {dir:?}; push the repo to a remote first")
        })?;

    let base_commit = git(dir, &["rev-parse", "HEAD"])?.trim().to_string();
    let branch = git(dir, &["rev-parse", "--abbrev-ref", "HEAD"])?.trim().to_string();
    // Detached HEAD: pin to the commit instead of the literal "HEAD".
    let reference = if branch == "HEAD" { base_commit.clone() } else { branch };

    // Tracked changes versus HEAD. Keep raw output (a patch needs its newlines).
    let diff = git(dir, &["diff", "HEAD"])?;

    // Untracked, non ignored files, tarred so they can be overlaid in the box.
    let untracked_list = git(dir, &["ls-files", "--others", "--exclude-standard"])?;
    let untracked_files: Vec<&str> = untracked_list.lines().filter(|l| !l.is_empty()).collect();
    let untracked_tar_gz = if untracked_files.is_empty() {
        None
    } else {
        Some(tar_files(dir, &untracked_files)?)
    };

    let dirty_overlay = if diff.trim().is_empty() && untracked_tar_gz.is_none() {
        None
    } else {
        Some(DirtyOverlay {
            diff,
            untracked_tar_gz,
            base_commit,
        })
    };

    Ok(GitWorkspaceSpec {
        repo_url,
        reference: Some(reference),
        depth: None,
        dirty_overlay,
        mount_path: None,
    })
}

/// Run a git command in `dir`, returning stdout. Errors on a non zero exit.
fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .with_context(|| format!("running git {}", args.join(" ")))?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Tar and gzip the given paths (relative to `dir`) into a byte buffer.
fn tar_files(dir: &Path, files: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new("tar")
        .arg("czf")
        .arg("-")
        .arg("-C")
        .arg(dir)
        .args(files)
        .output()
        .context("running tar for untracked files")?;
    if !out.status.success() {
        bail!("tar failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(out.stdout)
}
