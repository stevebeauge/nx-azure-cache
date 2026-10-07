# Azure Pipelines templates

Two step templates, **Linux agents only**:

- `nx-azure-cache-start.yml` starts the Gateway in the background, on behalf of the service
  connection, and waits until it is ready (60 s at most);
- `nx-azure-cache-report.yml` prints at the end of the job, even on failure, `status` (health,
  Identity, write state, per-Workspace stats) and the last 50 lines of the log, excluding the
  `/health` and `/stats` probes.

## Usage

```yaml
resources:
  repositories:
    - repository: nxcache
      type: github
      name: stevebeauge/nx-azure-cache
      endpoint: github # GitHub service connection

pool:
  vmImage: ubuntu-latest

steps:
  - checkout: self
  # A previous step puts the Linux binary (pnpm build:linux) on the agent: pipeline artifact,
  # file in the Workspace… An executable bit lost on the way is restored.
  - template: ci/nx-azure-cache-start.yml@nxcache
    parameters:
      binaryPath: $(Pipeline.Workspace)/nx-azure-cache/nx-azure-cache-linux-x64
      azureSubscription: nx-cache-ci # service connection name
      account: mystorageaccount
      # container: nx-cache (default)
  - script: pnpm exec nx affected -t lint test build
  - template: ci/nx-azure-cache-report.yml@nxcache
    parameters:
      binaryPath: $(Pipeline.Workspace)/nx-azure-cache/nx-azure-cache-linux-x64
```

The templates can also be copied into the Workspace: they depend on no other file. The
Workspace must declare the Activation plugin (see the
[README](../README.md#enable-the-cache-in-a-workspace)): it is what connects Nx to the Gateway.

## Prerequisites

- **Service connection** Azure Resource Manager with **workload identity federation**, created
  with the **Entra issuer** (the `vstoken.dev.azure.com` issuer is retired on 2027-07-01).
- Its principal holds a **write role on the container** (Storage Blob Data Contributor at
  container scope; see [`docs/runbook-azure.md`](../docs/runbook-azure.md)).
- Azure CLI on the agent (present on hosted agents): the start goes through `AzureCLI@2`.

## What the start step does

- The `AzureCLI@2` step provides the `AZURESUBSCRIPTION_*` variables and maps
  `SYSTEM_ACCESSTOKEN: $(System.AccessToken)`. The Gateway runs with `credential = pipelines`
  and requests a new `oidcToken` on each token renewal.
- If a Gateway already answers on the port (host shared by several agents: it belongs to
  another job), the step does not adopt it: a warning, `NX_AZURE_CACHE_DISABLED=1` set for the
  rest of the job, which runs on its local cache.
- `serve` is started in the background, stdout and stderr redirected to
  `$(Agent.TempDirectory)/nx-azure-cache.log`. It serves the following steps and the agent kills
  it at *Finalize Job*.
- If the Gateway is not ready after 60 s, or if the step fails: a warning, the reason
  (`status`), and the job goes on with the local cache (**Safe fallback**).
- No secret is written to the pipeline log. The Gateway process keeps `SYSTEM_ACCESSTOKEN` in
  its environment, readable by the agent's user: harmless on a hosted agent (disposable VM), to
  keep in mind on a shared agent.

Local test of the start script, outside Azure DevOps (Docker, and Python with PyYAML; a Gateway
that never becomes ready, then a second start that finds it already there):

```sh
pnpm build:linux && bash ci/test-start.sh
```

## Release pipelines

**Release pipelines do not include this template**: a `release` never reads the cache, so it
never ships a `dist` coming from the cache ([ADR 0001](../docs/adr/0001-trust-model.md)).
