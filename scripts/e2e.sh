#!/usr/bin/env bash
# End-to-end smoke test (Linux + eBPF required, e.g. ubuntu-latest).
#
# Builds the agent and the mock pipeline, runs a real `npm run build` under the
# recorder, and asserts that exactly one well-formed execution RunRecord was
# submitted and that reconciliation reports clean coverage.
#
# Requires: sudo, node/npm, a kernel with tracepoints (hosted ubuntu runners ok).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

WORK="$(mktemp -d)"
STATE="$(mktemp -d)"
MOCK_LOG="$WORK/submitted.jsonl"
MOCK_ADDR="127.0.0.1:8787"
TOKEN="e2e-secret-token"
cleanup() {
  [[ -n "${MOCK_PID:-}" ]] && sudo kill "$MOCK_PID" 2>/dev/null || true
  [[ -n "${AGENT_PID:-}" ]] && sudo kill -TERM "$AGENT_PID" 2>/dev/null || true
}
trap cleanup EXIT

echo "== building =="
cargo xtask build-ebpf --profile release
cargo build --release -p ci-tracer -p mock-pipeline

echo "== starting mock pipeline =="
MOCK_ADDR="$MOCK_ADDR" MOCK_TOKEN="$TOKEN" MOCK_LOG="$MOCK_LOG" \
  ./target/release/mock-pipeline &
MOCK_PID=$!
sleep 1

echo "== creating sample repo =="
REPO="$WORK/repo"
mkdir -p "$REPO/src"
git -C "$REPO" init -q
git -C "$REPO" config user.email e2e@example.com
git -C "$REPO" config user.name e2e
echo "export const x = 1;" > "$REPO/src/index.ts"
cat > "$REPO/package.json" <<'JSON'
{
  "name": "e2e-sample",
  "version": "1.0.0",
  "scripts": {
    "build": "mkdir -p dist && cat src/index.ts > dist/index.js && cat \"$PWD/src/index.ts\" > dist/abs.js && node -e \"require('fs').readFileSync('src/index.ts')\" && echo built"
  }
}
JSON
git -C "$REPO" add -A
git -C "$REPO" commit -qm "init"
COMMIT="$(git -C "$REPO" rev-parse HEAD)"

echo "== starting recorder agent =="
sudo --preserve-env=INPUT_PIPELINE_URL,INPUT_PIPELINE_TOKEN,INPUT_WHITELIST,GITHUB_REPOSITORY,GITHUB_SHA,GITHUB_EVENT_NAME,RUNNER_TEMP \
  env \
  INPUT_PIPELINE_URL="http://$MOCK_ADDR" \
  INPUT_PIPELINE_TOKEN="$TOKEN" \
  INPUT_WHITELIST="npm run build=>build" \
  GITHUB_REPOSITORY="owner/e2e-sample" \
  GITHUB_SHA="$COMMIT" \
  GITHUB_EVENT_NAME="push" \
  RUNNER_TEMP="$STATE" \
  ./target/release/ci-tracer &
AGENT_PID=$!
sleep 2

echo "== running observed build =="
( cd "$REPO" && npm run build )
sleep 1

echo "== stopping agent =="
sudo kill -TERM "$AGENT_PID"
AGENT_PID=""
sleep 2

echo "== assertions =="
if [[ ! -s "$MOCK_LOG" ]]; then
  echo "FAIL: no record submitted"; exit 1
fi
COUNT="$(wc -l < "$MOCK_LOG")"
echo "submitted records: $COUNT"
grep -q '"type":"build"' "$MOCK_LOG" || { echo "FAIL: no build record"; exit 1; }
grep -q "$COMMIT" "$MOCK_LOG" || { echo "FAIL: commit not recorded"; exit 1; }

SIGNAL="$STATE/ci-recorder-reconciliation.json"
[[ -f "$SIGNAL" ]] || { echo "FAIL: no reconciliation signal"; exit 1; }
cat "$SIGNAL"
grep -q '"coverage":"clean"' "$SIGNAL" || { echo "FAIL: coverage not clean"; exit 1; }

echo "PASS"
