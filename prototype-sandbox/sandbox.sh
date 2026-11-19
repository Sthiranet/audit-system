#!/usr/bin/env bash
# Builds the sthiranet-audit DAR and starts a Canton sandbox (one participant, one synchronizer,
# in-memory storage) with the JSON Ledger API v2 enabled and the DAR uploaded.
#
#   ./sandbox.sh                    # JSON Ledger API on http://localhost:7575
#   JSON_PORT=8080 ./sandbox.sh     # another port
#   ./sandbox.sh --canton-help      # extra flags are passed to `dpm sandbox`
#
# Ctrl-C stops it. Its state is in memory: a restarted sandbox has no parties, chains or anchors.
set -euo pipefail
cd "$(dirname "$0")"
JSON_PORT="${JSON_PORT:-7575}"
DAR=.daml/dist/sthiranet-audit-0.1.0.dar

dpm build
echo "Starting the Canton sandbox: JSON Ledger API on http://localhost:${JSON_PORT} (gRPC Ledger API on 6865)"
echo "Ready when 'curl -s localhost:${JSON_PORT}/v2/version' answers. Logs: prototype-sandbox/log/canton.log"
exec dpm sandbox --json-api-port "${JSON_PORT}" --dar "${DAR}" "$@"
