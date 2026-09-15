#!/usr/bin/env bash
set -euo pipefail

sudo chown vscode:vscode "$CARGO_TARGET_DIR"
git config --global --add safe.directory /workspaces/shorten
[ -f .env ] || cp .env.example .env

scripts/local-table.sh

echo "DynamoDB Local is at $AWS_ENDPOINT_URL_DYNAMODB; run: cargo test --workspace"
echo "Serve the functions with: cargo lambda watch --invoke-address 0.0.0.0 --invoke-port 9000"
