# Open source, distributed as a public npm package

The tool is published as open source under the MIT license, and distributed through a **public, unscoped npm package `nx-azure-cache`**. Every private distribution channel considered hit the same wall: authentication to add on every developer machine and in CI, plus an Azure Artifacts storage quota shared across the organization and already full. Going public makes the whole distribution anonymous.

The public repository is a fresh GitHub repository (`stevebeauge/nx-azure-cache`) started from a single initial commit of a cleaned-up tree: no history, no mention of the original environment (hosts, Workspaces, tenant, subscription, client ids, groups, storage account), no compiled `tenant_id` / `client_id` defaults. Everything is in English. The Azure procedure is generic: a script with mandatory parameters and a runbook with example values. The vocabulary settles before the first publication: the plugin option and the URL segment become `workspace` (`/{workspace}/v1/cache`).

Distribution:
- one npm package carrying the **Activation plugin** and a command; the **Gateway** binary comes from a per-platform sub-package (`win32-x64`, `linux-x64` musl) in `optionalDependencies`, filtered by `os`/`cpu`, **without `postinstall`**. It works with npm, pnpm and Bun; Yarn 2+ too, with its own config;
- the Workspace adds it to `devDependencies` and declares `"plugin": "nx-azure-cache"` in `nx.json`: no more copy under `tools/`;
- a single version for the plugin and the binaries, tag `vX.Y.Z`; GitHub Actions builds and publishes to npm (trusted publishing where possible, no secret).

Developer machine: `pnpm exec nx-azure-cache install --account … --tenant … --client-id …` copies itself to a fixed location (`%LOCALAPPDATA%\nx-azure-cache\bin`, `~/.local/bin`), stops the old Gateway process, replaces the binary, writes the settings to the profile's `config.toml` and restarts it. The same command serves first install and updates. There is no self-update: the plugin reports a Gateway older than its package and suggests the command, never failing a run; a newer Gateway triggers nothing, backward compatibility on `/health` covers it. Per-Workspace settings in `nx.json` are left aside until several settings scopes are needed.

CI: no more referenced templates. The start script ships in the package (`node_modules/nx-azure-cache/ci/start.sh`); the Workspace copies once from the README an `AzureCLI@2` step of about ten lines that calls it, plus a report step at the end of the job. The logic follows the lockfile version, and no GitHub connection has to be created in Azure DevOps projects.

Options ruled out:

| Option | Why it is ruled out |
|---|---|
| Moving the repository to Azure DevOps (dedicated project, tracker migration) | Pointless once the repository is public |
| Universal package or npm package on an Azure Artifacts feed | New auth on every machine (`vsts-npm-auth`, PAT on Linux, deferred 401s), and a shared organization quota |
| `git+https` dependency on an Azure DevOps distribution repository | Auth already in place on machines, but binaries committed, and uncertain cross-project access for the job token |
| Making the original private repository public | Its issues and pull requests would expose the original environment |
| winget | A private source means hosting a REST API |
| dotnet tool | Forces the .NET SDK on every machine |

Decided on 2026-10-07.
