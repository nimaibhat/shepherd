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

Early development. Building the local Docker vertical slice first (no cloud
account required), then E2B and Fly cloud providers. Track progress in
[PLAN.md section 11](./PLAN.md#11-milestones).

## Build

Requires a recent stable Rust toolchain and (for the Docker provider) a running
Docker daemon.

```sh
cargo build
cargo test
```

The `shepherd` binary is produced by the `cli` crate.

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
