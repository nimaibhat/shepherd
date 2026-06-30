//! Manual smoke test for the Docker provider. Requires a running Docker daemon.
//! Run with: cargo run -p shepherd-providers --example smoke

use std::collections::HashMap;

use shepherd_core::sandbox::{ExecOptions, SandboxProvider, SandboxSpec};
use shepherd_providers::DockerProvider;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let p = DockerProvider::connect()?;

    let mut labels = HashMap::new();
    labels.insert("test".to_string(), "smoke".to_string());

    println!("create...");
    let sbx = p
        .create(SandboxSpec {
            image: "alpine".to_string(),
            labels,
            ..Default::default()
        })
        .await?;
    let id = sbx.id.clone();
    println!("  -> {} {:?} {}", id, sbx.status, sbx.image);

    let cleanup = || async {
        let _ = p.destroy(&id).await;
        println!("destroyed {id}");
    };

    let run = async {
        println!("exec...");
        let r = p
            .exec(
                &id,
                &["sh".into(), "-c".into(), "echo hello from $(hostname)".into()],
                ExecOptions::default(),
            )
            .await?;
        println!("  exit {} stdout: {:?}", r.exit_code, r.stdout.trim());

        println!("putFile/getFile...");
        p.put_file(&id, "/tmp/test.txt", b"shepherd-was-here", 0o644).await?;
        let back = p.get_file(&id, "/tmp/test.txt").await?;
        println!("  getFile -> {:?}", String::from_utf8_lossy(&back));

        println!("list...");
        let mut filter = HashMap::new();
        filter.insert("test".to_string(), "smoke".to_string());
        let ls = p.list(&filter).await?;
        println!(
            "  found {} -> {}",
            ls.len(),
            ls.iter().map(|s| format!("{}:{:?}", s.id, s.status)).collect::<Vec<_>>().join(",")
        );

        println!("suspend/resume...");
        p.suspend(&id).await?;
        println!("  after suspend: {:?}", p.get(&id).await?.map(|s| s.status));
        p.resume(&id).await?;
        println!("  after resume: {:?}", p.get(&id).await?.map(|s| s.status));

        println!("OK");
        Ok::<(), anyhow::Error>(())
    };

    let result = run.await;
    cleanup().await;
    result
}
