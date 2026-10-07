#!/usr/bin/env bash
# Local test of the templates, outside Azure DevOps: extracts the scripts from both YAML files
# and runs them in ubuntu:24.04 with the musl binary (pnpm build:linux) and a Gateway that never
# becomes ready (pipelines credential without SYSTEM_OIDCREQUESTURI).
# Expected: ##vso warning, exit code 0, under 65 s, no secret in the output or the log.
# Then a second start, with the first Gateway still running (host shared by two jobs):
# warning, NX_AZURE_CACHE_DISABLED=1 set, no "Gateway ready".
# Prerequisites: Docker, Python 3 with PyYAML.
set -eu
export MSYS_NO_PATHCONV=1 # Git Bash: do not rewrite paths passed to docker
cd "$(dirname "$0")/.."
win() { cygpath -w "$1" 2>/dev/null || echo "$1"; }
py=$(command -v python || command -v python3)
[ -f dist/nx-azure-cache-linux-x64 ] || { echo "dist/nx-azure-cache-linux-x64 missing: pnpm build:linux"; exit 1; }

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
"$py" - "$(win "$tmp")" <<'EOF'
import sys, yaml
start = yaml.safe_load(open("ci/nx-azure-cache-start.yml", encoding="utf-8"))
report = yaml.safe_load(open("ci/nx-azure-cache-report.yml", encoding="utf-8"))
def write(name, text):
    open(sys.argv[1] + "/" + name, "w", encoding="utf-8", newline="\n").write(text)
write("start.sh", start["steps"][0]["inputs"]["inlineScript"])
write("report.sh", report["steps"][0]["bash"])
EOF

docker run --rm -v "$(win "$PWD/dist"):/b:ro" -v "$(win "$tmp"):/t:ro" \
  -e NX_AZURE_CACHE_BIN=/b/nx-azure-cache-linux-x64 \
  -e NX_AZURE_CACHE_ACCOUNT=mystorageaccount -e NX_AZURE_CACHE_CONTAINER=nx-cache \
  -e NX_AZURE_CACHE_CREDENTIAL=pipelines \
  -e SYSTEM_ACCESSTOKEN=system-token-sentinel-7f3a \
  -e AGENT_TEMPDIRECTORY=/agent-tmp \
  ubuntu:24.04 bash -c '
    set -u
    mkdir -p "$AGENT_TEMPDIRECTORY"
    t0=$SECONDS
    bash /t/start.sh >/out-start 2>&1; code=$?
    elapsed=$((SECONDS - t0))
    bash /t/report.sh >/out-report 2>&1
    bash /t/start.sh >/out-again 2>&1; code_again=$?
    echo "--- start output"; cat /out-start
    echo "--- second start output"; cat /out-again
    echo "--- report output"; cat /out-report
    echo "---"
    fail=0
    check() { if eval "$2"; then echo "ok    $1"; else echo "FAIL  $1"; fail=1; fi; }
    local_token=$(cat ~/.config/nx-azure-cache/local-token)
    check "exit code 0 (got: $code)" "[ $code -eq 0 ]"
    check "under 65 s (got: ${elapsed} s)" "[ $elapsed -lt 65 ]"
    check "##vso warning emitted" "grep -q \"^##vso\\[task.logissue type=warning\\]\" /out-start"
    check "Gateway still alive after the step" "\$NX_AZURE_CACHE_BIN status >/dev/null; [ \$? -eq 0 ]"
    disabled="^##vso\[task.setvariable variable=NX_AZURE_CACHE_DISABLED\]1\$"
    check "first start: remote cache not disabled" "! grep -q \"$disabled\" /out-start"
    check "second start: exit code 0 (got: $code_again)" "[ $code_again -eq 0 ]"
    check "second start: ##vso warning emitted" "grep -q \"^##vso\[task.logissue type=warning\]\" /out-again"
    check "second start: NX_AZURE_CACHE_DISABLED=1 set" "grep -q \"$disabled\" /out-again"
    check "second start: Gateway of another job not adopted" "! grep -q \"Gateway ready\" /out-again"
    for f in /out-start /out-report /out-again "$AGENT_TEMPDIRECTORY/nx-azure-cache.log"; do
      check "SYSTEM_ACCESSTOKEN absent from $f" "! grep -qF \"\$SYSTEM_ACCESSTOKEN\" $f"
      check "Local token absent from $f" "! grep -qF $local_token $f"
    done
    exit $fail'
