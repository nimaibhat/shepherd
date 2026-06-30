# Shepherd Plan

Run your AI coding agents in cloud sandboxes that keep working after you close
the laptop. Drive them from a terminal. Reconnect from anywhere.

Shepherd is an open source, terminal first control plane for persistent,
cloud hosted AI coding agents. You launch an agent (Claude Code today, others
later) into a cloud sandbox, it runs your task against the full codebase, and it
keeps going whether or not your machine is on. Your terminal is a thin viewport
that attaches and detaches at will.

This document is the source of truth for what we are building and why. Read it
top to bottom before touching code.

## Status (as of 2026-06-30)

What works and is tested on the local Docker provider, end to end:

- Core trait and types, Docker provider, git seeding with dirty overlay, headless
  Claude runner and stream-json parser, SQLite session store.
- `shepherd` CLI: `run` (with `--agent`), `ls`, `attach`, `logs`, `rm`, `serve`,
  and a default full-screen TUI.
- The agent runs DETACHED inside a tmux session in the box, so it survives the
  CLI exiting and the laptop powering off; reattach to the live session with
  `shepherd attach` (Ctrl-] detaches without killing it).
- Telegram messaging bridge (`shepherd serve`): `/ls`, `/use`, text a turn, the
  box is auto-resumed if it had auto-stopped, reply comes back to the chat.
- Full-screen ratatui TUI: workspaces grouped in a sidebar, live statuses
  (loading/working/idle/suspended/done/error) with a spinner, a detail+activity
  panel, footer keybindings.

What is validated against live Daytona (the cloud provider):

- create, exec, file put/get, list, suspend/resume, destroy, and connection_info
  (web terminal URL plus a minted SSH token). `shepherd run` seeding works on the
  default snapshot (it already has git). The Daytona provider is hand-rolled on
  the REST API (the daytona-client crate was dropped, it drifts from the API).

What is NOT done yet (good places to pick up):

- A cloud `--agent` run end to end: needs a Daytona snapshot with Node + claude
  (build from `images/base/Dockerfile`). The agent pipeline itself is validated
  on Docker; only the cloud image packaging remains.
- A fully interactive `ssh` attach session on Daytona (token mint is validated;
  the interactive terminal needs a real TTY to try).
- The persistence triad is only partially built: code persists via the per
  session git branch and the box's own disk, but the SessionStore transcript
  mirror (S3/Postgres) and the CLAUDE.md/artifact sync (section 4) are not built.
- Fan out (M9): multiple coordinated sessions.
- MCP config sync and OAuth surfacing (section 5, part of M8).
- The reconcile-back UX (git pull / PR review prompts in the TUI).
- Push notifications from the bridge (finish/error/needs-input).
- Embedded live terminal panes in the TUI (a vt100 multiplexer) were deferred on
  purpose until the cloud PTY story is settled; `attach` covers live terminals
  today.

## 1. The problem

Local coding agents (Claude Code, Codex, Cursor CLI) die when their process
dies: close the terminal, quit the editor, or power off the laptop, and the work
stops. Tools that "keep the agent alive" usually keep it alive on a machine you
own and have to keep on, which is the same problem wearing a hat.

We want the opposite property:

> Power off your computer completely. The agents keep running.

That is only possible if the agent's process and its entire world (code, memory,
conversation, tools) live somewhere that is not your machine, a cloud sandbox,
and your terminal is a disposable client.

## 2. The core design decision: invert ownership

The naive approach is to mirror your local filesystem up to the cloud (rsync,
Mutagen, Syncthing, a network mount). This is wrong for our goal:

- When the laptop is off, there is nothing to mirror from.
- An agent editing files in the cloud while your laptop edits the same files is a
  two way sync conflict nightmare.
- It chains the cloud's liveness to your machine being online, defeating the
  entire point.

So we invert it:

> The cloud sandbox holds the canonical workspace. Your laptop is a thin,
> disposable viewport. You reconcile back to local on your terms, via git, not
> via a constant background sync.

This reframes "emulate the user's filesystem" into two clean, solvable problems:
seed the workspace once, then persist and reconcile.

## 3. How the codebase and context get into the sandbox

Git is the transport, not a network mount.

### 3a. Seeding (at session create)
1. Clone the repo into the sandbox from its remote (git clone, shallow if
   large). This brings repo level context for free: CLAUDE.md, .claude/
   (commands, subagents, settings), and .mcp.json.
2. Dirty state overlay so the agent starts from exactly what you see, not just
   HEAD: capture `git diff HEAD` plus a tar of untracked but not ignored files at
   launch, ship it as a throwaway `wip/<session>` commit (or an object store
   bundle), and apply it on top of the clone. This is a one time point in time
   snapshot, not a live mirror.
3. Non git folders (degenerate case): tar, upload to object store, extract in box.

### 3b. The context that is not in the repo
| Layer | What | How it gets in |
|---|---|---|
| Code and repo config | source, repo CLAUDE.md, .claude/, .mcp.json | git clone plus dirty overlay |
| Global memory | ~/.claude/CLAUDE.md, auto memory, user MCP servers | opt in sync at create |
| Conversation state | session transcripts (~/.claude/projects/*.jsonl) | SessionStore (S3/Postgres), resume rehydrates the conversation |
| Auth and secrets | Anthropic key/OAuth, MCP creds | injected as sandbox secrets/env, never copied as files |

## 4. Persistence triad (the "laptop off" guarantee)

Three kinds of state must persist independently. Missing any one means the agent
wakes up with amnesia. (Per Anthropic's Agent SDK hosting docs, none of these
survive a container restart by default, and SessionStore mirrors transcripts
only.)

1. Session transcripts: a SessionStore adapter (SQLite/FS for dev,
   Postgres/S3/Redis for prod). Enables resume on a fresh box.
2. CLAUDE.md memory plus working dir artifacts: persistent volume and/or object
   store sync. SessionStore does NOT cover these.
3. The code itself: the agent commits to an `agent/<session>` branch every N
   steps. The diff is the durable output and the reconcile channel.

Idle cost control: snapshot or suspend the sandbox between active turns (Fly
suspend to disk, E2B/Morph snapshots) on a persistent volume, wake on event, not
a hot idle loop. "Always running" semantically, pennies in cost.

Reconcile back: when your laptop wakes, the TUI shows "N sessions advanced,
branch ready", and you git pull or review a PR. No background daemon, no
conflicts.

## 5. MCP in the sandbox

MCP works; transport decides feasibility.

- Remote/HTTP+SSE MCP (Linear, GitHub, Sentry, Supabase, most connectors): Claude
  Code dials a URL, the sandbox has egress, so these work as well as local, often
  better.
- stdio MCP scoped to the workspace/cloud (filesystem on repo, git, cloud
  Postgres): spawned as subprocesses inside the sandbox. Works, if the runtime is
  baked into the base image (Node, uv, Docker in Docker).
- stdio MCP bound to your physical laptop (local only files, keychain, a
  localhost dev service): unreachable when the laptop is off. Rehost in cloud or
  bridge only while online (graceful degradation).

Build implications, folded into existing layers:
- Config travels: project .mcp.json rides the git clone, user scoped servers sync
  with global memory.
- Runtime in the image: ship an opinionated base image with common MCP runtimes,
  not bare Ubuntu.
- Secrets injected: ${ENV_VAR} refs in MCP config map to sandbox secrets at boot,
  never written to files or git.
- Interactive OAuth gotcha: headless boxes cannot click "Allow". Prefer injected
  long lived tokens or device code flow, surface "MCP server X needs re-auth" in
  the TUI instead of failing silently.

## 6. Agent runtime

We drive headless Claude Code inside the sandbox, we do not reimplement the agent
loop:
- `claude -p "<task>" --output-format stream-json --verbose` for non interactive
  runs.
- Capture session_id, resume across invocations with `--resume <id>` or
  `--continue`.
- 1 session maps to 1 long lived claude subprocess (stdio). Concurrency per box
  is RAM bound: agents are roughly (RAM minus overhead) / per session ceiling.
  This sizes fan out.
- Provider agnostic: the agent is just a process in a Linux box with the repo and
  the right env, so swapping Claude Code for another CLI is a runner config, not
  a rewrite.

How it actually runs (built): the runner writes the invocation to a script and
launches it inside a detached tmux session named `shepherd` in the box, tee'ing
output to `.shepherd/agent.log`. tmux makes the agent reattachable (the herdr
"panes stay alive" model) and detaching never kills it; the log lets
`shepherd logs` work without an interactive terminal. The box runs as a non root
user (claude refuses `--dangerously-skip-permissions` as root) with the workspace
under that user's home (`/home/agent/workspace` on docker, `/home/daytona/...`
on Daytona).

## 7. Provider abstraction

A single SandboxProvider trait, with concrete adapters behind it. Anthropic's own
shortlist to evaluate: Modal, Cloudflare Sandboxes, Daytona, E2B, Fly Machines,
Vercel Sandbox (plus self hosted Docker, gVisor, Firecracker).

```rust
trait SandboxProvider {
    fn id(&self) -> ProviderId;
    async fn create(&self, spec: SandboxSpec) -> Result<Sandbox>;
    async fn get(&self, id: &SandboxId) -> Result<Option<Sandbox>>;
    async fn list(&self, labels: &HashMap<String, String>) -> Result<Vec<Sandbox>>;
    async fn exec(&self, id: &SandboxId, cmd: &[String], opts: ExecOptions) -> Result<ExecResult>;
    async fn attach_pty(&self, id: &SandboxId, cmd: &[String], opts: PtyOptions) -> Result<PtySession>;
    async fn connection_info(&self, id: &SandboxId) -> Result<ConnectionInfo>; // web/ssh
    async fn put_file(&self, id: &SandboxId, path: &str, content: &[u8], mode: u32) -> Result<()>;
    async fn get_file(&self, id: &SandboxId, path: &str) -> Result<Vec<u8>>;
    async fn snapshot(&self, id: &SandboxId) -> Result<String>;     // default: NotSupported
    async fn suspend(&self, id: &SandboxId) -> Result<()>;          // default: NotSupported
    async fn resume(&self, id: &SandboxId) -> Result<()>;           // default: NotSupported
    async fn destroy(&self, id: &SandboxId) -> Result<()>;
}
```

Two interactive models: local providers stream an in-process PTY via
`attach_pty` (docker); cloud providers expose `connection_info` (a web terminal
URL plus an ssh target) and `shepherd attach` uses the system ssh client. The
selected backend is chosen by `SHEPHERD_PROVIDER` (default `docker`).

Adapter roadmap and status:
1. docker (local), DONE and tested: develop and test the entire flow with zero
   cloud cost. suspend=pause, resume=unpause, snapshot=commit; interactive
   attach via the docker exec TTY stream.
2. daytona (cloud), DONE and validated live: the chosen first cloud target.
   Hand-rolled on the Daytona REST API with reqwest and lenient parsing (the
   daytona-client crate was dropped, it drifts from the live API). suspend=stop,
   resume=start, snapshot=backup; auto-stop (20m) and auto-archive (1d) cost
   guardrails; attach via web terminal URL + minted ssh token.
3. Later, behind the same trait: E2B, Fly Machines, Modal, Cloudflare.

## 8. Architecture

```
+--------------+   attach/detach (websocket)   +----------------------+
| shepherd CLI |<----------------------------->| control plane         |
| (thin TUI)   |                               |  session registry      |
+--------------+                               |  auth, routing         |
   panes, logs,                                |  wake/suspend          |
   reattach                                    |  secret injection      |
                                               +----------+-----------+
                                                          | SandboxProvider
                        +---------------------------------+---------------------------------+
                        v                                 v                                 v
                 +------------+                    +------------+                    +------------+
                 | Sandbox A  |                    | Sandbox B  |                    | Sandbox C  |
                 | claude -p  |                    | claude     |                    |  fan out   |
                 | repo clone |                    | --resume   |                    |            |
                 +-----+------+                    +-----+------+                    +-----+------+
                       +-------------- SessionStore (transcripts) -----------------------+
                              plus volume/object store (memory, code) plus git branches
```

For the MVP the control plane is a local daemon the CLI talks to. It is designed
to be hoisted into a hosted multi tenant service later without changing the CLI
contract.

## 9. Tech stack

- Language: Rust. Single static binary in the spirit of herdr, strong typing for
  the provider trait surface, good async ecosystem.
- Async runtime: tokio.
- Docker provider: bollard (async Docker Engine API client).
- Daytona provider: hand-rolled on reqwest (json + multipart) with lenient serde,
  no third-party Daytona SDK.
- CLI parsing: clap.
- TUI: ratatui plus crossterm (event-stream). Reattach is done via a tmux session
  inside the box (the box's own multiplexer), reached over the docker exec TTY
  stream locally or ssh / web terminal on cloud. No portable-pty.
- Telegram bridge: reqwest long polling (getUpdates), no webhook or open ports.
- Serialization: serde and serde_json (stream-json parsing, state files).
- Errors: thiserror in the core crate, anyhow at the binary boundary.
- Persistence (dev): SQLite (rusqlite, bundled) at `~/.shepherd/state.sqlite`.
  Pluggable to Postgres later (the Store surface is small).
- Fully custom. No dependency on or coupling to the herdr product (tmux inside
  the box is a generic in-box tool, not herdr).

Not yet built from this list: the websocket CLI-to-daemon transport and the
object store for transcripts/artifacts. Today the CLI talks to providers
directly; there is no separate daemon process yet.

## 10. Repo layout

```
shepherd/
  Cargo.toml            workspace manifest
  PLAN.md               this file
  images/base/          Dockerfile for the agent image (git, node, claude, tmux)
  docs/                 daytona.md, mobile.md
  crates/
    core/               domain types, SandboxProvider trait, Session, errors
    providers/          adapters: docker (bollard), daytona (reqwest)
                        examples/: smoke, daytona_smoke, daytona_prune, seed
    agent/              ClaudeRunner, stream-json parser, git seeding, capture
    cli/                the `shepherd` binary
                        main.rs (run/ls/attach/logs/rm/serve/tui dispatch),
                        store.rs (sqlite), attach.rs, bot.rs (telegram),
                        tui.rs (ratatui board)
```

## 11. Milestones

Status markers: [done], [partial], [todo].

- M0 [done] Scaffold: workspace, plan, tooling.
- M1 [done] Core contracts: SandboxProvider, Session, WorkspaceSpec, AgentRunner.
- M2 [done] Docker provider: create/exec/attach_pty/put-get/snapshot/suspend/
  destroy against local Docker.
- M3 [done] Git seeding: clone plus dirty state overlay into a sandbox.
- M4 [done] Headless Claude runner: claude -p, capture session id, resume, parse
  stream-json.
- M5 [partial] Session store: SQLite session registry done. The transcript
  mirror (resume across a fresh box via S3/Postgres) is NOT done.
- M6 [done] CLI plus attach: run / ls / attach / logs / rm; agent runs detached
  in tmux; reattach with attach. Vertical slice proven.
- M7 [done] Cloud provider: Daytona (not E2B), validated live. Always on,
  snapshot/suspend, cost guardrails.
- M8 [partial] Secrets plus MCP: secret injection (ANTHROPIC_API_KEY) done and
  the base image carries MCP runtimes. MCP config sync and OAuth surfacing: todo.
- M9 [todo] Fan out: multiple coordinated sessions.
- M10 [partial] Cloud and mobile attach: web terminal URL + ssh-token attach for
  Daytona done; a fully interactive ssh session not yet exercised live.
- M11 [done] Messaging bridge: Telegram `shepherd serve` with /ls, /use, and
  text-a-turn (auto-resume + reply). Push notifications: todo.
- M12 [done] TUI: herdr-style full-screen session board (workspaces, live
  statuses, detail+activity, keybindings).
- M13 [todo] Persistence triad completion: transcript mirror + CLAUDE.md/artifact
  sync, plus the reconcile-back UX (section 4).
- M14 [todo] Embedded live terminal panes in the TUI (vt100 multiplexer), once
  the cross-provider PTY story is settled.

## 12. Mobile and messaging control

Because every box already runs in the cloud and survives the laptop being off,
the phone does not need to reach your machine at all, it reaches the control
plane. Two complementary paths:

1. Mobile attach (M10), herdr parity. Run a phone SSH client (Blink, Termius)
   against the control plane and use `shepherd attach <id>`. The session is a
   cloud box, so this works with the laptop fully off, which herdr cannot do
   (herdr's box is wherever its binary runs, often your machine).

2. Messaging bridge (M11), the "text it from my phone" experience. A persistent
   bot endpoint on the control plane maps a chat to a session:
   - You text a prompt. The bridge injects it as a turn into the headless agent
     (`claude -p --resume <agent_session_id>`) in that session's sandbox.
   - The agent works in the cloud. stream-json output is summarized and relayed
     back to the chat; long output links to the full transcript.
   - Push notifications fire when a run finishes, hits an error, or needs input
     (for example an interactive MCP re-auth, see section 5). This is the part
     herdr has no answer for.

   Telegram first: free, bidirectional, rich bot API, no SMS or phone number
   cost, easy push. SMS (Twilio) and Slack are later adapters behind the same
   bridge interface. The bridge is a thin control channel, not a full UI; the
   terminal stays the primary surface (see non goals).

   This is feasible precisely because of the architecture we already chose:
   agents run 24/7 in the cloud, headless Claude resumes by session id, and the
   control plane is a long lived service that can host a webhook. Prior art
   exists: agent-deck's "conductor" sessions already relay to Telegram/Slack and
   escalate to a human.

## 13. Non goals (for now)

- Reimplementing the agent loop (we drive Claude Code headless).
- A web UI (terminal first, web can come later).
- Multi tenant billing/hosting (the daemon is single user first, designed to
  hoist later).
- Live bidirectional filesystem sync (explicitly rejected, see section 2).
