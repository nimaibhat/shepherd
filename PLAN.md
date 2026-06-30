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

## 7. Provider abstraction

A single SandboxProvider trait, with concrete adapters behind it. Anthropic's own
shortlist to evaluate: Modal, Cloudflare Sandboxes, Daytona, E2B, Fly Machines,
Vercel Sandbox (plus self hosted Docker, gVisor, Firecracker).

```rust
trait SandboxProvider {
    async fn create(&self, spec: SandboxSpec) -> Result<Sandbox>;
    async fn exec(&self, id: &SandboxId, cmd: &[String], opts: ExecOptions) -> Result<ExecResult>;
    async fn attach_pty(&self, id: &SandboxId, cmd: &[String], opts: PtyOptions) -> Result<PtySession>;
    async fn put_file(&self, id: &SandboxId, path: &str, content: &[u8]) -> Result<()>;
    async fn get_file(&self, id: &SandboxId, path: &str) -> Result<Vec<u8>>;
    async fn snapshot(&self, id: &SandboxId) -> Result<String>;
    async fn suspend(&self, id: &SandboxId) -> Result<()>;
    async fn resume(&self, id: &SandboxId) -> Result<()>;
    async fn destroy(&self, id: &SandboxId) -> Result<()>;
}
```

Adapter roadmap (priority order):
1. docker (local): develop and test the entire flow with zero cloud cost or
   accounts. First adapter.
2. e2b: Firecracker microVM isolation, open source, cheap, agent purpose built.
   First cloud target.
3. fly: Machines REST API, best suspend to disk plus volumes for cheap always on
   idle.
4. Later: Daytona, Modal, Cloudflare.

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
- CLI parsing: clap.
- TUI: ratatui plus crossterm. Reattachable PTY via portable-pty locally and the
  Docker exec attach stream for boxes.
- Serialization: serde and serde_json (stream-json parsing, state files).
- Errors: thiserror in library crates, anyhow at the binary boundary.
- Transport: websocket (CLI to daemon). Reconnect with exponential backoff plus
  jitter (500ms, double, cap 30s) and a stateless resume protocol.
- Persistence (dev): SQLite plus local FS object store, pluggable to
  Postgres/S3/Redis.
- Fully custom. No dependency on or coupling to any existing multiplexer.

## 10. Repo layout

```
shepherd/
  Cargo.toml            workspace manifest
  PLAN.md               this file
  crates/
    core/               domain types, SandboxProvider trait, Session model
    providers/          adapters: docker (dev), e2b, fly
    agent/              headless Claude runner, stream-json parser, git seeding
    cli/                the `shepherd` binary: CLI plus TUI plus local daemon
```

## 11. Milestones

- M0 Scaffold: workspace, plan, tooling.
- M1 Core contracts: SandboxProvider, Session, WorkspaceSpec, AgentRunner types.
- M2 Docker provider: create/exec/attach_pty/put-get/snapshot/suspend/destroy
  against local Docker. The whole loop, no cloud.
- M3 Git seeding: clone plus dirty state overlay into a sandbox.
- M4 Headless Claude runner: claude -p, capture session id, resume, parse
  stream-json.
- M5 SessionStore (sqlite/fs): persist and resume transcripts across boxes.
- M6 CLI plus attach: `shepherd run`, `shepherd ls`, `shepherd attach` with a
  reattachable PTY. Vertical slice: laptop off, agent running, reattach later.
- M7 Cloud provider (E2B): first real always on target plus snapshot/suspend.
- M8 Secrets plus MCP: secret injection, MCP config sync, OAuth surfacing.
- M9 Fan out: multiple sessions, status board, branch per session.
- M10 Cloud and mobile attach: wire interactive attach for the cloud provider
  and reach a running session from a phone terminal app (herdr parity, but the
  box is in the cloud). Daytona exposes both a PTY API and SSH access for this;
  the Rust crate is REST only, so this needs a small websocket or SSH bridge.
- M11 Messaging bridge: a chat bot (Telegram first) bound to sessions, so you
  text a prompt from your phone and the cloud agent works and replies, plus push
  notifications when an agent finishes or needs input.

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
