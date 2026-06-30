# Running on Daytona (cloud, survives power-off)

The Daytona provider runs sandboxes in Daytona's cloud, so a session keeps
working after you shut your laptop. This is the first backend that delivers the
core promise; the local Docker provider still needs your machine on.

Status: the adapter is implemented against `daytona-client` 0.5 but has not yet
been validated against the live service. Treat this as a first run-through and
expect to file rough edges. See the NOTE comments in
`crates/providers/src/daytona/provider.rs`.

## 1. Get an API key

Sign up at https://www.daytona.io and create an API key in the dashboard.

## 2. Create the agent snapshot

Daytona boots sandboxes from snapshots, not raw Docker images. Build a snapshot
that has the agent toolchain (git, Node, and the claude CLI). The source is
[`images/base/Dockerfile`](../images/base/Dockerfile).

Use the Daytona dashboard or CLI to create a snapshot from that Dockerfile (see
the Daytona snapshots docs: https://www.daytona.io/docs). Name it, for example,
`shepherd-base`. Shepherd passes the value of `--image` straight through as the
Daytona snapshot name.

## 3. Set the environment

```sh
export SHEPHERD_PROVIDER=daytona
export DAYTONA_API_KEY=...           # from step 1
export ANTHROPIC_API_KEY=...         # forwarded into the sandbox as a secret
# optional, for self-hosted Daytona:
# export DAYTONA_BASE_URL=https://your-daytona/api
```

## 4. Launch a session

```sh
shepherd run --repo . --image shepherd-base --agent --prompt "your task"
```

This captures your repo (including uncommitted changes), creates a Daytona
sandbox from the snapshot, seeds the workspace, injects your key, and runs
headless Claude. You can now close your laptop; the run continues in the cloud.

Check on it later:

```sh
shepherd ls          # status of your sessions
```

## Known gaps (cloud)

- `shepherd attach` is not wired for Daytona yet. Interactive cloud terminals
  (and the mobile attach path, PLAN.md M10) are the next cloud milestone. For
  now a `--agent` run is fire-and-forget: it survives power-off and you watch it
  with `ls`.
- The default workspace mount is `/workspace`. If the Daytona sandbox user
  cannot write there, set a different mount when this is wired through, or adjust
  the snapshot. This is one of the things to confirm on the first live run.
