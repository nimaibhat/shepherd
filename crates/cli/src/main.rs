//! The `shepherd` binary: launch and manage persistent cloud sandbox agents.

mod attach;
mod store;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use shepherd_agent::{capture_local_workspace, seed_workspace, ClaudeRunner, ClaudeStreamParser};
use shepherd_core::agent::{AgentEvent, RunRequest};
use shepherd_core::ids::SessionId;
use shepherd_core::sandbox::{ExecOptions, SandboxProvider, SandboxResources, SandboxSpec};
use shepherd_core::session::{default_branch_for, Session, SessionStatus};
use shepherd_core::workspace::WorkspaceSpec;
use shepherd_providers::{DaytonaProvider, DockerProvider};

use store::Store;

/// Default base image for seeding only. Has git, sh, and tar. Lightweight, used
/// when you are not launching the agent.
const DEFAULT_IMAGE: &str = "alpine/git";

/// Image used when `--agent` is set: adds Node and the claude CLI. Build it with
/// images/base/build.sh.
const AGENT_IMAGE: &str = "shepherd-base:latest";

/// Secret env vars forwarded from the local environment into the sandbox when
/// running the agent. Injected as env, never written to files (PLAN.md section 5).
const FORWARDED_SECRETS: &[&str] = &["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"];

const SESSION_LABEL: &str = "shepherd.session";

#[derive(Parser)]
#[command(name = "shepherd", version, about = "Persistent cloud sandbox AI coding agents")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Launch a session: seed a sandbox from a local repo and register it.
    Run {
        /// Path to the local git repo to seed from.
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Task prompt. Required with --agent.
        #[arg(long)]
        prompt: Option<String>,
        /// Human readable session title.
        #[arg(long)]
        title: Option<String>,
        /// Base image for the sandbox.
        #[arg(long, default_value = DEFAULT_IMAGE)]
        image: String,
        /// After seeding, launch the headless agent with --prompt. Requires the
        /// agent image and an ANTHROPIC_API_KEY in the environment.
        #[arg(long)]
        agent: bool,
    },
    /// Attach an interactive terminal to a session's sandbox.
    Attach {
        /// Session id.
        session: String,
    },
    /// Show the agent's output log for a session (for detached --agent runs).
    Logs {
        /// Session id.
        session: String,
        /// Print the raw log instead of parsed events.
        #[arg(long)]
        raw: bool,
    },
    /// List sessions and their live sandbox status.
    Ls,
    /// Tear down a session and its sandbox.
    Rm {
        /// Session id.
        session: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let store = Store::open(&state_db_path())?;
    let provider = make_provider()?;
    let provider = provider.as_ref();

    match cli.command {
        Command::Run { repo, prompt, title, image, agent } => {
            run(&store, provider, &repo, prompt, title, &image, agent).await
        }
        Command::Attach { session } => attach::attach(&store, provider, &session).await,
        Command::Logs { session, raw } => logs(&store, provider, &session, raw).await,
        Command::Ls => ls(&store, provider).await,
        Command::Rm { session } => rm(&store, provider, &session).await,
    }
}

/// Select the sandbox backend. Today only the local Docker provider is wired;
/// cloud providers (E2B, Fly) plug in here behind the same trait. Override with
/// the SHEPHERD_PROVIDER env var.
fn make_provider() -> Result<Box<dyn SandboxProvider>> {
    let name = std::env::var("SHEPHERD_PROVIDER").unwrap_or_else(|_| "docker".to_string());
    match name.as_str() {
        "docker" => {
            let p = DockerProvider::connect()
                .context("could not connect to Docker; is the Docker daemon running?")?;
            Ok(Box::new(p))
        }
        "daytona" => {
            let p = DaytonaProvider::from_env()
                .context("set DAYTONA_API_KEY (and optionally DAYTONA_BASE_URL) to use the daytona provider")?;
            Ok(Box::new(p))
        }
        other => anyhow::bail!(
            "unknown SHEPHERD_PROVIDER '{other}'; use 'docker' or 'daytona'"
        ),
    }
}

#[allow(clippy::too_many_arguments)]
async fn run(
    store: &Store,
    provider: &dyn SandboxProvider,
    repo: &Path,
    prompt: Option<String>,
    title: Option<String>,
    image: &str,
    agent: bool,
) -> Result<()> {
    if agent && prompt.is_none() {
        anyhow::bail!("--agent requires --prompt");
    }
    // Choose the agent image automatically unless the user pinned one.
    let image = if agent && image == DEFAULT_IMAGE { AGENT_IMAGE } else { image };

    // Gather secrets to inject as env (never written to files).
    let mut secret_env = HashMap::new();
    if agent {
        for key in FORWARDED_SECRETS {
            if let Ok(val) = std::env::var(key) {
                secret_env.insert(key.to_string(), val);
            }
        }
        if secret_env.is_empty() {
            anyhow::bail!(
                "--agent needs credentials; set ANTHROPIC_API_KEY (or ANTHROPIC_AUTH_TOKEN) in your environment"
            );
        }
    }

    let repo = repo.canonicalize().with_context(|| format!("repo path {repo:?}"))?;
    println!("capturing workspace from {repo:?} ...");
    let mut git_spec = capture_local_workspace(&repo)?;
    // Place the workspace where the provider's default user can actually write.
    // Daytona sandboxes run as the `daytona` user, so / is not writable.
    if git_spec.mount_path.is_none() {
        git_spec.mount_path = Some(default_mount_for(&provider.id(), agent).to_string());
    }
    let dirty = git_spec
        .dirty_overlay
        .as_ref()
        .map(|d| {
            let untracked = d.untracked_tar_gz.as_ref().map_or(0, |t| t.len());
            format!("diff {}B, untracked {}B", d.diff.len(), untracked)
        })
        .unwrap_or_else(|| "clean".to_string());
    println!("  repo {}  ref {}  ({dirty})", git_spec.repo_url, git_spec.reference.clone().unwrap_or_default());

    let session_id = SessionId::new();
    let branch = default_branch_for(&session_id);
    let title = title.or_else(|| prompt.clone()).unwrap_or_else(|| "session".to_string());

    let mut labels = HashMap::new();
    labels.insert(SESSION_LABEL.to_string(), session_id.to_string());

    // Give agent boxes more headroom than the 1 vCPU / 1 GB default so node and
    // claude do not get starved or OOM-killed.
    let resources = if agent {
        SandboxResources { cpus: Some(2.0), memory_mb: Some(2048), disk_mb: None }
    } else {
        SandboxResources::default()
    };

    println!("creating sandbox ({image}) ...");
    let sandbox = provider
        .create(SandboxSpec {
            image: image.to_string(),
            labels,
            env: secret_env,
            resources,
        })
        .await?;

    // Persist the session before seeding so a crash mid-seed is still visible.
    // Store the workspace WITHOUT the (already applied) overlay to keep it lean.
    let lean_workspace = WorkspaceSpec::Git(shepherd_core::workspace::GitWorkspaceSpec {
        dirty_overlay: None,
        ..git_spec.clone()
    });
    let mut session = Session {
        id: session_id.clone(),
        title,
        status: SessionStatus::Seeding,
        provider_id: provider.id(),
        sandbox_id: Some(sandbox.id.clone()),
        workspace: lean_workspace,
        agent_session_id: None,
        branch: branch.clone(),
        created_at: now(),
        updated_at: now(),
        error: None,
    };
    store.upsert(&session)?;

    println!("seeding workspace ...");
    let seed_spec = WorkspaceSpec::Git(git_spec);
    match seed_workspace(provider, &sandbox.id, &seed_spec, &branch).await {
        Ok(mount) => {
            session.status = SessionStatus::Idle;
            session.updated_at = now();
            store.upsert(&session)?;
            println!();
            println!("session {session_id} ready");
            println!("  sandbox {}  workspace {mount}  branch {branch}", sandbox.id);

            if agent {
                launch_agent(store, provider, &mut session, &sandbox.id, &mount, &prompt.unwrap_or_default()).await?;
            } else {
                println!("  attach: shepherd attach {session_id}");
            }
            Ok(())
        }
        Err(e) => {
            session.status = SessionStatus::Error;
            session.error = Some(e.to_string());
            session.updated_at = now();
            store.upsert(&session)?;
            // Box left around for inspection; remove with `shepherd rm`.
            Err(e.into())
        }
    }
}

/// Launch the headless agent DETACHED inside the box so it keeps running after
/// the CLI exits or the laptop powers off. Watch it later with `shepherd logs`.
async fn launch_agent(
    store: &Store,
    provider: &dyn SandboxProvider,
    session: &mut Session,
    sandbox_id: &shepherd_core::ids::SandboxId,
    mount: &str,
    prompt: &str,
) -> Result<()> {
    let log_path = agent_log_path(mount);
    let runner = ClaudeRunner::default();
    let req = RunRequest {
        sandbox_id: sandbox_id.clone(),
        prompt: prompt.to_string(),
        cwd: mount.to_string(),
        resume_agent_session_id: session.agent_session_id.clone(),
        allowed_tools: Vec::new(),
        env: HashMap::new(),
    };
    let script = runner.detached_launch(&req, &log_path);
    let res = provider
        .exec(
            sandbox_id,
            &["sh".into(), "-c".into(), script],
            ExecOptions { cwd: Some(mount.to_string()), env: HashMap::new() },
        )
        .await?;

    session.status = SessionStatus::Running;
    session.updated_at = now();
    store.upsert(session)?;

    println!("  agent launched in background (pid {})", res.stdout.trim());
    println!("  watch:  shepherd logs {}", session.id);
    println!("  (you can close your laptop now; it keeps running in the sandbox)");
    Ok(())
}

/// Show a session's detached agent log, parsed into events (or raw with --raw).
async fn logs(store: &Store, provider: &dyn SandboxProvider, session: &str, raw: bool) -> Result<()> {
    let id: SessionId = session.into();
    let Some(mut s) = store.get(&id)? else {
        anyhow::bail!("no such session: {session}");
    };
    let Some(sandbox_id) = s.sandbox_id.clone() else {
        anyhow::bail!("session {session} has no sandbox");
    };
    let mount = s.workspace.mount_path().to_string();
    let bytes = match provider.get_file(&sandbox_id, &agent_log_path(&mount)).await {
        Ok(b) => b,
        Err(_) => {
            println!("no agent log yet (launch one with: shepherd run --agent --prompt ...)");
            return Ok(());
        }
    };

    if raw {
        use std::io::Write;
        std::io::stdout().write_all(&bytes).ok();
        return Ok(());
    }

    let text = String::from_utf8_lossy(&bytes);
    let mut parser = ClaudeStreamParser::new();
    let mut events = parser.feed(&text);
    events.extend(parser.flush());
    if events.is_empty() {
        // Not stream-json yet (e.g. an early error). Show what we have.
        print!("{text}");
    } else {
        for ev in &events {
            print_event(ev);
        }
    }

    // Capture the agent session id for later resume, once it appears.
    if s.agent_session_id.is_none() {
        if let Some(sid) = parser.agent_session_id() {
            s.agent_session_id = Some(sid.to_string());
            store.upsert(&s)?;
        }
    }
    Ok(())
}

/// Convention for where a session's detached agent writes its log inside the box.
fn agent_log_path(mount: &str) -> String {
    format!("{mount}/.shepherd/agent.log")
}

fn print_event(ev: &AgentEvent) {
    match ev {
        AgentEvent::Session { agent_session_id } => println!("  [session {agent_session_id}]"),
        AgentEvent::Text { text } => println!("{text}"),
        AgentEvent::ToolUse { name, .. } => println!("  [tool: {name}]"),
        AgentEvent::ToolResult { name, ok } => {
            println!("  [tool {name}: {}]", if *ok { "ok" } else { "error" })
        }
        AgentEvent::Error { message } => eprintln!("  [error: {message}]"),
        AgentEvent::Done { exit_code } => println!("  [done: exit {exit_code}]"),
    }
}

async fn ls(store: &Store, provider: &dyn SandboxProvider) -> Result<()> {
    let sessions = store.list()?;
    if sessions.is_empty() {
        println!("no sessions. start one with: shepherd run --repo <path>");
        return Ok(());
    }
    println!("{:<20} {:<10} {:<24} {:<22} {}", "SESSION", "STATUS", "BRANCH", "SANDBOX", "TITLE");
    for s in sessions {
        let live = match &s.sandbox_id {
            Some(id) => provider
                .get(id)
                .await
                .ok()
                .flatten()
                .map(|sb| format!("{:?}", sb.status))
                .unwrap_or_else(|| "gone".to_string()),
            None => "-".to_string(),
        };
        let sandbox = s.sandbox_id.as_ref().map(|i| i.to_string()).unwrap_or_default();
        println!(
            "{:<20} {:<10} {:<24} {:<22} {}",
            s.id.to_string(),
            live,
            s.branch,
            sandbox,
            s.title
        );
    }
    Ok(())
}

async fn rm(store: &Store, provider: &dyn SandboxProvider, session: &str) -> Result<()> {
    let id: SessionId = session.into();
    let Some(s) = store.get(&id)? else {
        anyhow::bail!("no such session: {session}");
    };
    if let Some(sandbox_id) = &s.sandbox_id {
        provider.destroy(sandbox_id).await?;
    }
    store.delete(&id)?;
    println!("removed session {session}");
    Ok(())
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Where to clone the workspace, where the box's user can actually write.
/// Daytona runs as `daytona`; the agent image (shepherd-base) runs as `agent`;
/// the plain seeding image runs as root, so / is fine there.
fn default_mount_for(provider_id: &str, agent: bool) -> &'static str {
    match provider_id {
        "daytona" => "/home/daytona/workspace",
        _ if agent => "/home/agent/workspace",
        _ => "/workspace",
    }
}

fn state_db_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".shepherd").join("state.sqlite")
}
