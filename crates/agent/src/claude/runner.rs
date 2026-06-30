//! Drives headless Claude Code inside a sandbox. We do not reimplement the agent
//! loop; this builds a `claude -p ... --output-format stream-json` invocation,
//! runs it via the provider, and parses the output into AgentEvents.
//!
//! Note: the current SandboxProvider::exec returns output at completion, so
//! events are emitted once the process exits. A streaming exec hook (for live
//! output) lands when the TUI is wired up (PLAN.md M6); the events-channel
//! contract here stays the same.

use async_trait::async_trait;
use tokio::sync::mpsc;

use shepherd_core::agent::{AgentEvent, AgentRunner, RunRequest, RunResult};
use shepherd_core::errors::Result;
use shepherd_core::sandbox::{ExecOptions, SandboxProvider};

use super::stream_parser::ClaudeStreamParser;

pub struct ClaudeRunner {
    /// Path to the claude binary inside the sandbox.
    pub bin: String,
    /// Pass --dangerously-skip-permissions. Reasonable in an isolated sandbox;
    /// the box is throwaway and has no access to the user's machine.
    pub skip_permissions: bool,
    /// Optional model override (--model).
    pub model: Option<String>,
}

impl Default for ClaudeRunner {
    fn default() -> Self {
        Self {
            bin: "claude".to_string(),
            skip_permissions: true,
            model: None,
        }
    }
}

impl ClaudeRunner {
    /// The agent invocation as a single shell line, with each argv element
    /// quoted. The CLI runs this inside a reattachable tmux session in the box so
    /// the run survives disconnects and you can reattach to the live terminal
    /// from anywhere (see `shepherd attach`).
    pub fn command_line(&self, req: &RunRequest) -> String {
        self.build_command(req)
            .iter()
            .map(|a| sh_quote(a))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn build_command(&self, req: &RunRequest) -> Vec<String> {
        let mut cmd = vec![
            self.bin.clone(),
            "-p".into(),
            req.prompt.clone(),
            "--output-format".into(),
            "stream-json".into(),
            "--verbose".into(),
        ];
        if let Some(id) = &req.resume_agent_session_id {
            cmd.push("--resume".into());
            cmd.push(id.clone());
        }
        if !req.allowed_tools.is_empty() {
            cmd.push("--allowedTools".into());
            cmd.push(req.allowed_tools.join(","));
        }
        if let Some(model) = &self.model {
            cmd.push("--model".into());
            cmd.push(model.clone());
        }
        if self.skip_permissions {
            cmd.push("--dangerously-skip-permissions".into());
        }
        cmd
    }
}

/// Single-quote a shell argument, escaping embedded single quotes.
fn sh_quote(arg: &str) -> String {
    let safe = !arg.is_empty()
        && arg
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'/' | b':' | b'=' | b','));
    if safe {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', r#"'\''"#))
    }
}

#[async_trait]
impl AgentRunner for ClaudeRunner {
    fn name(&self) -> &str {
        "claude-code"
    }

    async fn run(
        &self,
        provider: &dyn SandboxProvider,
        req: RunRequest,
        events: mpsc::Sender<AgentEvent>,
    ) -> Result<RunResult> {
        let command = self.build_command(&req);
        let result = provider
            .exec(
                &req.sandbox_id,
                &command,
                ExecOptions {
                    cwd: Some(req.cwd.clone()),
                    env: req.env.clone(),
                },
            )
            .await?;

        let mut parser = ClaudeStreamParser::new();
        let mut parsed = parser.feed(&result.stdout);
        parsed.extend(parser.flush());
        for ev in parsed {
            let _ = events.send(ev).await;
        }
        let _ = events
            .send(AgentEvent::Done {
                exit_code: result.exit_code,
            })
            .await;

        Ok(RunResult {
            agent_session_id: parser.agent_session_id().map(str::to_string),
            exit_code: result.exit_code,
        })
    }
}
