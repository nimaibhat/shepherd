# Shepherd

Run your AI coding agents in cloud sandboxes that keep working after you close
the laptop. Drive them from a terminal. Reconnect from anywhere.

Shepherd is an open source, terminal first control plane for persistent,
cloud hosted AI coding agents. Launch an agent (Claude Code today) into a cloud
sandbox; it runs your task against the full codebase and keeps going whether or
not your machine is on. Your terminal is a thin viewport that attaches and
detaches at will.

> Power off your computer completely. The agents keep running.

## Why

Local coding agents die when their process dies. Close the terminal or shut the
lid and the work stops. Shepherd moves the agent and its entire world (code,
memory, conversation, tools) into a cloud sandbox, so your machine becomes
optional. Unlike filesystem mirroring approaches, the sandbox owns the canonical
workspace and you reconcile back via git on your terms.

See [PLAN.md](./PLAN.md) for the full architecture and roadmap.

## Status

Early development. The local Docker vertical slice works today: capture a repo,
seed a sandbox, register the session, and attach an interactive terminal that
you can detach from and reattach to. Cloud providers (E2B, Fly) and live
headless Claude execution are next. Track progress in
[PLAN.md section 11](./PLAN.md#11-milestones).

What works now: `run`, `ls`, `attach`, `rm` against local Docker.
Not yet: running the agent (claude) automatically, and surviving a full
power-off (that needs a cloud provider, M7). For now the box lives in your
local Docker, so the machine stays on.

## Prerequisites

- A recent stable Rust toolchain (`rustup`).
- A running Docker daemon (Docker Desktop on macOS).
- The repo you point at must have a git `origin` remote that is pushed, since
  seeding clones from the remote and overlays your uncommitted changes.

## Install

```sh
cargo install --path crates/cli   # installs the `shepherd` binary to ~/.cargo/bin
```

Or build without installing:

```sh
cargo build --release             # binary at target/release/shepherd
cargo test                        # run the unit tests
```

## Use

```sh
shepherd run --repo .             # seed a sandbox from the current repo
shepherd ls                       # list sessions and live sandbox status
shepherd attach <session-id>      # interactive terminal in the box; Ctrl-] to detach
shepherd rm <session-id>          # tear down the sandbox and forget the session
```

Session state lives at `~/.shepherd/state.sqlite`.

### Cloud (survives power-off)

Set `SHEPHERD_PROVIDER=daytona` to run sandboxes in Daytona's cloud so sessions
keep working after you shut the laptop. See [docs/daytona.md](./docs/daytona.md)
for setup. The Daytona adapter is implemented but not yet validated against the
live service; the local Docker path is the tested one today.

## Architecture in one breath

```
shepherd CLI (thin TUI) <-> control daemon <-> SandboxProvider <-> cloud sandbox
                                                                    running claude -p
                            plus SessionStore (transcripts)
                            plus volume/object store (memory, code)
                            plus git branch per session (reconcile)
```

## License

MIT, see [LICENSE](./LICENSE).
