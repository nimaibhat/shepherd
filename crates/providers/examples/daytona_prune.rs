//! Destroy all shepherd-managed Daytona sandboxes. Requires DAYTONA_API_KEY.
//! Run with: cargo run -p shepherd-providers --example daytona_prune

use std::collections::HashMap;

use shepherd_core::sandbox::SandboxProvider;
use shepherd_providers::DaytonaProvider;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let p = DaytonaProvider::from_env()?;
    let managed = p.list(&HashMap::new()).await?;
    println!("found {} managed sandbox(es)", managed.len());
    for sb in managed {
        print!("  destroying {} ({:?}) ... ", sb.id, sb.status);
        match p.destroy(&sb.id).await {
            Ok(()) => println!("ok"),
            Err(e) => println!("error: {e}"),
        }
    }
    Ok(())
}
