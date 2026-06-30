//! The `shepherd` binary: launch and manage persistent cloud sandbox agents.

mod attach;
mod store;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use shepherd_agent::{capture_local_workspace, seed_workspace, ClaudeRunner};
use shepherd_core::agent::{AgentEvent, AgentRunner, RunRequest};
use shepherd_core::ids::SessionId;
use shepherd_core::sandbox::{SandboxProvider, SandboxSpec};
use shepherd_core::session::{default_branch_for, Session, SessionStatus};
use shepherd_core::workspace::WorkspaceSpec;
use shepherd_providers::DockerProvider;

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
    let provider = DockerProvider::connect().context(
        "could not connect to Docker; is the Docker daemon running?",
    )?;

    match cli.command {
        Command::Run { repo, prompt, title, image, agent } => {
            run(&store, &provider, &repo, prompt, title, &image, agent).await
        }
        Command::Attach { session } => attach::attach(&store, &provider, &session).await,
        Command::Ls => ls(&store, &provider).await,
        Command::Rm { session } => rm(&store, &provider, &session).await,
    }
}

#[allow(clippy::too_many_arguments)]
async fn run(
    store: &Store,
    provider: &DockerProvider,
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
    let git_spec = capture_local_workspace(&repo)?;
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

    println!("creating sandbox ({image}) ...");
    let sandbox = provider
        .create(SandboxSpec {
            image: image.to_string(),
            labels,
            env: secret_env,
            ..Default::default()
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
                run_agent(store, provider, &mut session, &sandbox.id, &mount, &prompt.unwrap_or_default()).await?;
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

/// Launch the headless agent in the seeded box and stream its events.
async fn run_agent(
    store: &Store,
    provider: &DockerProvider,
    session: &mut Session,
    sandbox_id: &shepherd_core::ids::SandboxId,
    mount: &str,
    prompt: &str,
) -> Result<()> {
    session.status = SessionStatus::Running;
    session.updated_at = now();
    store.upsert(session)?;

    println!("  running agent ...");
    println!();

    let (tx, mut rx) = tokio::sync::mpsc::channel::<AgentEvent>(256);
    let printer = tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            print_event(&ev);
        }
    });

    let runner = ClaudeRunner::default();
    let req = RunRequest {
        sandbox_id: sandbox_id.clone(),
        prompt: prompt.to_string(),
        cwd: mount.to_string(),
        resume_agent_session_id: session.agent_session_id.clone(),
        allowed_tools: Vec::new(),
        env: HashMap::new(),
    };
    let result = runner.run(provider, req, tx).await;
    let _ = printer.await;

    match result {
        Ok(run) => {
            session.agent_session_id = run.agent_session_id;
            session.status = SessionStatus::Idle;
            session.updated_at = now();
            store.upsert(session)?;
            println!();
            println!("  agent finished (exit {}). reattach: shepherd attach {}", run.exit_code, session.id);
            Ok(())
        }
        Err(e) => {
            session.status = SessionStatus::Error;
            session.error = Some(e.to_string());
            session.updated_at = now();
            store.upsert(session)?;
            Err(e.into())
        }
    }
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

async fn ls(store: &Store, provider: &DockerProvider) -> Result<()> {
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

async fn rm(store: &Store, provider: &DockerProvider, session: &str) -> Result<()> {
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

fn state_db_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".shepherd").join("state.sqlite")
}
