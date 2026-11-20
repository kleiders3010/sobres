#!/usr/bin/env bash
# Run the whole workspace test suite (engine unit + simulation tests, tasks domain + document epochs).
set -euo pipefail
cd "$(dirname "$0")/.."
cargo test --workspace "$@"
