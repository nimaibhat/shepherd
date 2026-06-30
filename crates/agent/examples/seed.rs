//! Manual test for git seeding. Requires a running Docker daemon and network.
//! Run with: cargo run -p shepherd-agent --example seed

use shepherd_agent::seed_workspace;
use shepherd_core::sandbox::{ExecOptions, SandboxProvider, SandboxSpec};
use shepherd_core::workspace::{GitWorkspaceSpec, WorkspaceSpec};
use shepherd_providers::DockerProvider;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let p = DockerProvider::connect()?;

    println!("create box (alpine/git)...");
    let sbx = p
        .create(SandboxSpec {
            image: "alpine/git".to_string(),
            ..Default::default()
        })
        .await?;
    let id = sbx.id.clone();
    println!("  -> {id} {:?}", sbx.status);

    let branch = "agent/seed-test";
    let spec = WorkspaceSpec::Git(GitWorkspaceSpec {
        repo_url: "https://github.com/octocat/Hello-World.git".to_string(),
        reference: None,
        depth: Some(1),
        dirty_overlay: None,
        mount_path: None,
    });

    let run = async {
        println!("seed...");
        let mount = seed_workspace(&p, &id, &spec, branch).await?;
        println!("  seeded at {mount}");

        let b = p
            .exec(&id, &git(&mount, &["branch", "--show-current"]), ExecOptions::default())
            .await?;
        println!("  current branch: {:?}", b.stdout.trim());

        let ls = p
            .exec(&id, &["ls".into(), "-1".into(), mount.clone()], ExecOptions::default())
            .await?;
        println!("  files: {:?}", ls.stdout.split_whitespace().collect::<Vec<_>>());

        let log = p
            .exec(&id, &git(&mount, &["log", "--oneline", "-1"]), ExecOptions::default())
            .await?;
        println!("  head: {:?}", log.stdout.trim());

        assert_eq!(b.stdout.trim(), branch, "branch should be the seeded session branch");
        println!("OK");
        Ok::<(), anyhow::Error>(())
    };

    let result = run.await;
    let _ = p.destroy(&id).await;
    println!("destroyed {id}");
    result
}

fn git(dir: &str, args: &[&str]) -> Vec<String> {
    let mut v = vec!["git".to_string(), "-C".to_string(), dir.to_string()];
    v.extend(args.iter().map(|s| s.to_string()));
    v
}
