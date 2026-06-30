# Running on Daytona (cloud, survives power-off)

The Daytona provider runs sandboxes in Daytona's cloud, so a session keeps
working after you shut your laptop. This is the first backend that delivers the
core promise; the local Docker provider still needs your machine on.

Status: the adapter is implemented against the (community) `daytona-client` 0.5
crate but has not yet been validated against the live service. Two known risks
to confirm on the first real run:

- The crate calls Daytona's older `/toolbox/{id}/toolbox/...` exec and file
  endpoints, which the current API marks deprecated. They should still work; if
  not, the fix is to hand-roll those calls against the current API.
- General first-run roughness. See the NOTE comments in
  `crates/providers/src/daytona/provider.rs`.

## 1. Get an API key

Create a key at https://app.daytona.io/dashboard/keys and export it:

```sh
export DAYTONA_API_KEY=...
```

## 2. Build the agent snapshot

Daytona boots sandboxes from snapshots (reusable templates baked from an image
definition), not raw Docker images, and the unofficial Rust crate only supports
the snapshot path. So create a snapshot that has the agent toolchain (git, Node,
the claude CLI). The source image is
[`images/base/Dockerfile`](../images/base/Dockerfile).

Easiest is the Daytona CLI or one of the official SDKs. For example, with the
Python SDK:

```python
from daytona import Daytona, CreateSnapshotParams, Image

daytona = Daytona()  # uses DAYTONA_API_KEY
daytona.snapshot.create(CreateSnapshotParams(
    name="shepherd-base",
    image=Image.from_dockerfile("images/base/Dockerfile"),
))
```

Shepherd passes the value of `--image` straight through as the snapshot name, so
name it something you will reuse (e.g. `shepherd-base`). See the Daytona snapshot
docs for the CLI equivalent: https://www.daytona.io/docs.

## 3. Set the environment

```sh
export SHEPHERD_PROVIDER=daytona
export DAYTONA_API_KEY=...            # from step 1
export ANTHROPIC_API_KEY=...          # forwarded into the sandbox as a secret
# optional, for self-hosted Daytona:
# export DAYTONA_BASE_URL=https://your-daytona/api
```

## 4. Launch a session

```sh
shepherd run --repo . --image shepherd-base --agent --prompt "your task"
```

This captures your repo (including uncommitted changes), creates a Daytona
sandbox from the snapshot, seeds the workspace into `/home/daytona/workspace`,
injects your key, and runs headless Claude. You can now close your laptop; the
run continues in the cloud. Check on it later with `shepherd ls`.

## Good to know (from the Daytona platform)

- Network: free tiers (1 and 2) restrict sandbox network access to a whitelist,
  but that whitelist already includes the Anthropic API and GitHub/GitLab, so
  cloning from GitHub and running Claude work even on the free tier. Reaching
  arbitrary external URLs needs Tier 3+.
- Resources: default sandbox is 1 vCPU / 1 GB RAM / 3 GiB disk; max is 4 vCPU /
  8 GB / 10 GB. Shepherd sizes memory and disk in GB.
- Cost while idle: `suspend` maps to Daytona `stop` (frees CPU and RAM, keeps
  disk); `resume` maps to `start`. Archived sandboxes have no quota impact.

## Known gaps (cloud)

- `shepherd attach` is not wired for Daytona yet. Daytona does expose a PTY API
  and SSH access, which are the path for interactive cloud terminals and the
  mobile attach goal (PLAN.md M10), but the Rust crate is REST only, so this
  needs a small websocket/SSH bridge. For now a `--agent` run is fire-and-forget:
  it survives power-off and you watch it with `ls`.
