# Shepherd base sandbox image.
#
# Has everything seeding and a headless Claude Code run need: git, a shell, tar,
# Node, and the claude CLI. Richer MCP runtimes (uv for Python servers, etc) can
# be layered on top of this. The container is kept alive by Shepherd overriding
# the entrypoint, so this image sets no long running CMD of its own.
FROM node:22-bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        git \
        ca-certificates \
        curl \
        tar \
    && rm -rf /var/lib/apt/lists/*

# The headless agent we drive (PLAN.md section 6).
RUN npm install -g @anthropic-ai/claude-code

WORKDIR /workspace
