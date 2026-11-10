# Sourced by start.sh and report.sh: Gateway binary, log file and annotations of the CI system
# (Azure Pipelines or GitHub Actions). Linux agents, after `pnpm install`.

if [ "${GITHUB_ACTIONS:-}" = true ]; then
  ci=github
  log="${RUNNER_TEMP:-/tmp}/nx-azure-cache.log"
  warn() { echo "::warning::nx-azure-cache: $1"; }
  # Read by the next steps of the job, not by this one.
  disable() { echo NX_AZURE_CACHE_DISABLED=1 >>"$GITHUB_ENV"; }
else
  ci=pipelines
  log="${AGENT_TEMPDIRECTORY:-/tmp}/nx-azure-cache.log"
  warn() { echo "##vso[task.logissue type=warning]nx-azure-cache: $1"; }
  disable() { echo "##vso[task.setvariable variable=NX_AZURE_CACHE_DISABLED]1"; }
fi

# The binary of the Platform package installed next to this package, resolved like the plugin
# does (NX_AZURE_CACHE_BIN wins): the CI runs the Gateway of the lockfile's version.
bin=$(node -e 'process.stdout.write(require(require("path").resolve(process.argv[1])).binaryPath(process.env) || "")' \
  "$(dirname "${BASH_SOURCE[0]}")/../index.cjs" 2>/dev/null)
