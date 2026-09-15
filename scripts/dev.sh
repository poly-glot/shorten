#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

if [ -f .env ]; then
    set -a
    . ./.env
    set +a
fi

export AWS_ACCESS_KEY_ID="${AWS_ACCESS_KEY_ID:-local}"
export AWS_ENDPOINT_URL_DYNAMODB="${AWS_ENDPOINT_URL_DYNAMODB:-http://localhost:8000}"
export AWS_REGION="${AWS_REGION:-eu-west-2}"
export AWS_SECRET_ACCESS_KEY="${AWS_SECRET_ACCESS_KEY:-local}"
export METRIC_NAMESPACE="${METRIC_NAMESPACE:-shorten}"
export PUBLIC_BASE_URL="${PUBLIC_BASE_URL:-http://localhost:3000}"
export TABLE_NAME="${TABLE_NAME:-shorten-local}"
unset ORIGIN_VERIFY

cargo lambda --version >/dev/null 2>&1 || { echo "cargo-lambda not found: open this repo in the dev container, or install rustup and then pip install cargo-lambda ziglang" >&2; exit 1; }

PIDS=()
trap 'kill "${PIDS[@]}" 2>/dev/null' EXIT

scripts/local-table.sh

cargo lambda watch --invoke-address 0.0.0.0 --invoke-port 9000 &
PIDS+=($!)

echo "waiting for the functions to compile…"
until curl -sS -o /dev/null --max-time 900 http://localhost:9000/lambda-url/mgmt/links; do sleep 3; done

scripts/seed.sh

python3 frontend/serve.py &
PIDS+=($!)
echo "ready: http://localhost:3000"
wait
