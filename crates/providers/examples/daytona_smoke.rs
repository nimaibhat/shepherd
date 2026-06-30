//! Manual smoke test for the Daytona provider. Requires DAYTONA_API_KEY.
//! Run with: cargo run -p shepherd-providers --example daytona_smoke

use std::collections::HashMap;

use shepherd_core::sandbox::{ExecOptions, SandboxProvider, SandboxSpec};
use shepherd_providers::DaytonaProvider;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let p = DaytonaProvider::from_env()?;

    let mut labels = HashMap::new();
    labels.insert("test".to_string(), "smoke".to_string());

    println!("create (default snapshot)...");
    let sbx = p
        .create(SandboxSpec {
            image: String::new(), // empty -> Daytona default snapshot
            labels,
            ..Default::default()
        })
        .await?;
    let id = sbx.id.clone();
    println!("  -> {} {:?} {}", id, sbx.status, sbx.image);

    let run = async {
        println!("exec...");
        let r = p
            .exec(&id, &["sh".into(), "-c".into(), "echo hello from $(hostname); whoami; pwd".into()], ExecOptions::default())
            .await?;
        println!("  exit {} out: {:?}", r.exit_code, r.stdout.trim());

        println!("git available?");
        let g = p.exec(&id, &["sh".into(), "-c".into(), "git --version 2>&1 || echo NO-GIT".into()], ExecOptions::default()).await?;
        println!("  {:?}", g.stdout.trim());

        println!("putFile/getFile...");
        p.put_file(&id, "/home/daytona/test.txt", b"shepherd-was-here", 0o644).await?;
        let back = p.get_file(&id, "/home/daytona/test.txt").await?;
        println!("  getFile -> {:?}", String::from_utf8_lossy(&back));

        println!("list...");
        let mut filter = HashMap::new();
        filter.insert("test".to_string(), "smoke".to_string());
        let ls = p.list(&filter).await?;
        println!("  found {}", ls.len());

        println!("connection_info (web terminal + ssh token)...");
        let conn = p.connection_info(&id).await?;
        println!("  web: {:?}", conn.web_terminal_url);
        println!("  ssh: {}", conn.ssh_target.as_deref().map(mask).unwrap_or_else(|| "NONE".into()));

        println!("OK");
        Ok::<(), anyhow::Error>(())
    };

    let result = run.await;
    println!("destroy...");
    let _ = p.destroy(&id).await;
    println!("destroyed {id}");
    result
}

/// Mask a credential so we confirm it exists without printing it.
fn mask(s: &str) -> String {
    if let Some((tok, host)) = s.split_once('@') {
        let shown = tok.chars().take(6).collect::<String>();
        format!("{shown}...@{host}")
    } else {
        "***".into()
    }
}
