#!/usr/bin/env bash
# Local test of the CI scripts shipped in the npm package (npm/nx-azure-cache/ci/), outside any
# CI system: lays out node_modules as `pnpm install` would on a Linux agent, with the musl binary
# (pnpm build:linux) as the Platform package, and runs the scripts in node:22-bookworm exactly as the
# README snippets do, once as Azure Pipelines and once as GitHub Actions. The Gateway never
# becomes ready (no reachable OIDC endpoint).
# Expected for each: warning annotation, exit code 0, under 65 s, the binary found through the
# package (no NX_AZURE_CACHE_BIN), the CI's Identity selected, no secret in the output or the log.
# Then a second start, with the first Gateway still running (host shared by two jobs): warning,
# NX_AZURE_CACHE_DISABLED=1 set for the next steps, no "Gateway ready".
# Prerequisites: Docker.
set -eu
export MSYS_NO_PATHCONV=1 # Git Bash: do not rewrite paths passed to docker
cd "$(dirname "$0")/.."
win() { cygpath -w "$1" 2>/dev/null || echo "$1"; }
[ -f dist/nx-azure-cache-linux-x64 ] || { echo "dist/nx-azure-cache-linux-x64 missing: pnpm build:linux"; exit 1; }

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/ws/node_modules/@nx-azure-cache/linux-x64"
cp -r npm/nx-azure-cache "$tmp/ws/node_modules/nx-azure-cache"
cp npm/linux-x64/package.json "$tmp/ws/node_modules/@nx-azure-cache/linux-x64/"
cp dist/nx-azure-cache-linux-x64 "$tmp/ws/node_modules/@nx-azure-cache/linux-x64/nx-azure-cache"
chmod +x "$tmp/ws/node_modules/@nx-azure-cache/linux-x64/nx-azure-cache"

cat >"$tmp/check.sh" <<'EOF'
set -u
cd /ws
start=node_modules/nx-azure-cache/ci/start.sh
report=node_modules/nx-azure-cache/ci/report.sh
if [ "$KIND" = github ]; then
  warning='^::warning::'; tmpdir=$RUNNER_TEMP; secret=$ACTIONS_ID_TOKEN_REQUEST_TOKEN
  disabled() { grep -qx 'NX_AZURE_CACHE_DISABLED=1' "$GITHUB_ENV"; }
  mkdir -p "$RUNNER_TEMP"; : >"$GITHUB_ENV"
else
  warning='^##vso\[task.logissue type=warning\]'; tmpdir=$AGENT_TEMPDIRECTORY; secret=$SYSTEM_ACCESSTOKEN
  disabled() { grep -q '^##vso\[task.setvariable variable=NX_AZURE_CACHE_DISABLED\]1$' /out-again; }
  mkdir -p "$AGENT_TEMPDIRECTORY"
fi
t0=$SECONDS
bash $start >/out-start 2>&1; code=$?
elapsed=$((SECONDS - t0))
bash $report >/out-report 2>&1; code_report=$?
bash $start >/out-again 2>&1; code_again=$?
echo "--- start output"; cat /out-start
echo "--- second start output"; cat /out-again
echo "--- report output"; cat /out-report
echo "---"
fail=0
check() { if eval "$2"; then echo "ok    [$KIND] $1"; else echo "FAIL  [$KIND] $1"; fail=1; fi; }
bin=node_modules/@nx-azure-cache/linux-x64/nx-azure-cache
local_token=$(cat ~/.config/nx-azure-cache/local-token 2>/dev/null)
check "Local token created by the Gateway" "[ -n '$local_token' ]"
check "exit code 0 (got: $code)" "[ $code -eq 0 ]"
check "under 65 s (got: ${elapsed} s)" "[ $elapsed -lt 65 ]"
check "warning emitted" "grep -q '$warning' /out-start"
check "Gateway still alive after the step" "$bin status >/dev/null"
check "log written to the CI temp directory" "[ -s $tmpdir/nx-azure-cache.log ]"
check "Identity of the CI selected ($KIND)" "grep -q '^Identity: *$KIND\$' /out-report"
check "report: exit code 0 (got: $code_report)" "[ $code_report -eq 0 ]"
check "report: log tail printed" "grep -q 'Last 50 lines' /out-report"
check "first start: remote cache not disabled" "! grep -q DISABLED /out-start"
check "second start: exit code 0 (got: $code_again)" "[ $code_again -eq 0 ]"
check "second start: warning emitted" "grep -q '$warning' /out-again"
check "second start: NX_AZURE_CACHE_DISABLED=1 set" "disabled"
check "second start: Gateway of another job not adopted" "! grep -q 'Gateway ready' /out-again"
for f in /out-start /out-report /out-again "$tmpdir/nx-azure-cache.log"; do
  check "CI secret absent from $f" "! grep -qF '$secret' $f"
  check "Local token absent from $f" "! grep -qF '$local_token' $f"
done
exit $fail
EOF

run() {
  docker run --rm -v "$(win "$tmp"):/t:ro" -e NX_AZURE_CACHE_ACCOUNT=mystorageaccount "$@" \
    node:22-bookworm bash -c 'cp -r /t/ws /ws && bash /t/check.sh'
}
fail=0
run -e KIND=pipelines -e TF_BUILD=True -e AGENT_TEMPDIRECTORY=/agent-tmp \
  -e SYSTEM_ACCESSTOKEN=system-token-sentinel-7f3a || fail=1
run -e KIND=github -e GITHUB_ACTIONS=true -e CI=true -e RUNNER_TEMP=/runner-tmp \
  -e GITHUB_ENV=/runner-tmp/github-env \
  -e ACTIONS_ID_TOKEN_REQUEST_URL=http://127.0.0.1:9/token \
  -e ACTIONS_ID_TOKEN_REQUEST_TOKEN=actions-token-sentinel-4c1e \
  -e AZURE_CLIENT_ID=00000000-0000-0000-0000-000000000001 \
  -e AZURE_TENANT_ID=00000000-0000-0000-0000-000000000002 || fail=1
exit $fail
