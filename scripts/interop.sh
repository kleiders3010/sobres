#!/usr/bin/env bash
# Old/new interoperability (principle 8, dependency update policy): an OLDER build reads
# the NEWEST golden corpus. (The current build reading every older corpus runs in the
# normal test suite: kt-tasks tests/corpus.rs.)
# Usage: scripts/interop.sh <old git ref>   e.g. scripts/interop.sh v0.1.0
set -euo pipefail
OLD=${1:?old git ref}
ROOT=$(git rev-parse --show-toplevel)
WT="$ROOT/../tasklist-interop-$(date +%s)"
git -C "$ROOT" worktree add --detach "$WT" "$OLD" >/dev/null
trap 'git -C "$ROOT" worktree remove --force "$WT" >/dev/null 2>&1 || true' EXIT
echo "== $OLD reading the corpus of $(git -C "$ROOT" rev-parse --short HEAD)"
# The main target dir: dependencies are reused, only our crates rebuild.
( cd "$WT" && CARGO_TARGET_DIR="$ROOT/target" KT_CORPUS_DIR="$ROOT/corpus" cargo test -q -p kt-tasks --test corpus reads_every_corpus_generation )
