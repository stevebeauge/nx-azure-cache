#!/usr/bin/env bash
# nx-azure-cache report at the end of the job, even on failure: status (health, Identity, write
# state, per-Workspace stats) and the log tail. Place it after the Nx steps. Never fails.
. "$(dirname "$0")/env.sh"

[ -n "$bin" ] && "$bin" status
echo
# Without the /health and /stats probes (startup wait, plugin, status) that drown the rest.
echo "Last 50 lines of $log, excluding /health and /stats:"
grep -vE 'GET "/(health|stats)" ' "$log" 2>/dev/null | tail -n 50
exit 0
