#!/usr/bin/env bash
# Starts the nx-azure-cache Gateway in the background for the rest of the job, then waits until
# it is ready (60 s at most). Azure Pipelines (AzureCLI@2 step) or GitHub Actions, Linux agents.
# Never fails the job (Safe fallback): on any problem, a warning and the job runs on its local
# cache. Snippets to copy: README, "CI".
. "$(dirname "$0")/env.sh"

if [ -z "$bin" ]; then
  warn "no Gateway binary for this agent (is nx-azure-cache installed? Linux x64 only), the job continues on the local cache"
  exit 0
fi
chmod +x "$bin" 2>/dev/null # in case the package manager dropped the executable bit

# The Identity of the CI system, unless the step chose another one.
export NX_AZURE_CACHE_CREDENTIAL="${NX_AZURE_CACHE_CREDENTIAL:-$ci}"

# Host shared by several agents: a Gateway already running belongs to another job
# (another Identity, killed when that job ends). This job stays on its local cache.
if "$bin" status >/dev/null 2>&1; then
  warn "a Gateway is already running on this port (another job?), remote cache disabled for this job"
  disable
  exit 0
fi

# Streams redirected to a file: a child holding the step's pipes makes it linger, then the agent
# kills the process tree. The agent kills the Gateway at the end of the job (Azure Pipelines:
# VSTS_PROCESS_LOOKUP_ID inherited; GitHub Actions: orphan process clean-up).
nohup "$bin" serve </dev/null >>"$log" 2>&1 &
pid=$!

deadline=$((SECONDS + 60))
while [ "$SECONDS" -lt "$deadline" ]; do
  if "$bin" status --json 2>/dev/null | grep -q '"ready":true'; then
    echo "nx-azure-cache: Gateway ready (log: $log)"
    exit 0
  fi
  kill -0 "$pid" 2>/dev/null || break # serve exited: no point waiting
  sleep 1
done

warn "Gateway not ready, the job continues on the local cache. Reason below, log: $log"
"$bin" status 2>&1
exit 0
