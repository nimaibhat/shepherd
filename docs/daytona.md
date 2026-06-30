# Running on Daytona (cloud, survives power-off)

The Daytona provider runs sandboxes in Daytona's cloud, so a session keeps
working after you shut your laptop. This is the first backend that delivers the
core promise; the local Docker provider still needs your machine on.

Status: validated against the live service. Create, exec, file put/get, list,
suspend/resume, destroy, and interactive connection info (web terminal URL plus
a minted SSH token) all work end to end. The provider is hand-rolled on the
Daytona REST API with reqwest (the community daytona-client crate was dropped:
it deserialized responses into strict structs that drift from the live API).

Good news from validation: the Daytona default snapshot already includes git, so
`shepherd run` seeding (clone plus dirty overlay) works without building a custom
snapshot. You only need a custom snapshot to run the agent (`--agent`), since
that also needs Node and the claude CLI.

## 1. Get an API key

Create a key at https://app.daytona.io/dashboard/keys and export it:

```sh
export DAYTONA_API_KEY=...
```

## 2a. Seeding only (no custom snapshot needed)

If you just want to seed a repo into a cloud box and work in it (no agent), the
default snapshot already has git, so pass an empty image:

```sh
SHEPHERD_PROVIDER=daytona shepherd run --repo . --image "" --title "my session"
```

## 2b. Build the agent snapshot (for --agent)

To run the agent, the snapshot also needs Node and the claude CLI. Build a
snapshot from [`images/base/Dockerfile`](../images/base/Dockerfile) (which has
git, Node, claude, and tmux).

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

## Interactive access

`shepherd attach <session>` works on Daytona: it prints the web terminal URL
(open from a phone, no app) and drops you into the box over the system `ssh`
client (minting a short-lived SSH token), where the agent's tmux session is
reattachable. See [mobile.md](./mobile.md).

## Known gaps (cloud)

- Validated: lifecycle, seeding, files, and connection info (web terminal URL
  plus SSH token mint). Not yet exercised live: an end-to-end `--agent` run on a
  custom snapshot, and a fully interactive `ssh` attach session (the token mint
  is confirmed; the interactive terminal needs a real TTY to try).
