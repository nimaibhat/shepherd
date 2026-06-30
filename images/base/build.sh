#!/usr/bin/env bash
# Build the Shepherd base sandbox image used by `shepherd run --agent`.
set -euo pipefail
dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
tag="${1:-shepherd-base:latest}"
echo "building $tag from $dir/Dockerfile ..."
docker build -t "$tag" "$dir"
echo "done: $tag"
