# nx-azure-cache

> **Work in progress.** This project is under active development: the API, the configuration
> and the distribution (npm package, install steps) are not stable yet and may change without
> notice.

Remote cache for Nx monorepos, stored in Azure Blob Storage and served to Nx by a Gateway local
to each machine. Vocabulary: [`GLOSSARY.md`](GLOSSARY.md); decisions: [`docs/adr/`](docs/adr/);
contributing: [`CONTRIBUTING.md`](CONTRIBUTING.md).

## Build and run

Prerequisites: stable Rust (`rustup`), Node.js and pnpm (for the `package.json` scripts, which
install no dependency).

```sh
pnpm build   # binary in target/release/nx-azure-cache(.exe)
pnpm test
```

### Static Linux binary

From Windows (or any machine with Docker):

```sh
pnpm build:linux   # → dist/nx-azure-cache-linux-x64
```

- Built in `rust:1.95-alpine` (musl target, hence static); only Docker is required.
- The cargo registry and the Linux `target/` live in the Docker volumes
  `nx-azure-cache-cargo-registry` and `nx-azure-cache-target-linux`: an unchanged rebuild takes
  a few seconds. `docker volume rm` both to start cold.
- `--locked`: `Cargo.lock` must be up to date.
- The script then checks that `ldd` reports the binary as static and that `version` runs in
  bare `debian:bookworm-slim` and `ubuntu:24.04`.
- TLS: `rustls` only, no OpenSSL (the image lacks its headers: a dependency on `openssl-sys`
  fails the build). CA roots are the system's (`/etc/ssl/certs`): hosted Ubuntu agents have
  them, a bare `debian:bookworm-slim` does not (`apt-get install ca-certificates`).

Run the Gateway in the foreground:

```sh
target/release/nx-azure-cache serve
target/release/nx-azure-cache version
```

- It listens on `127.0.0.1` only, port `7484` by default.
- One instance per machine: a second `serve` on the same port prints "already running" and
  exits with code 0. If the port is held by another program, it exits with code 1.
- `GET http://127.0.0.1:7484/health` answers without a token.
- Each request produces a log line (see [Troubleshooting](#troubleshooting)).

### Config

Optional TOML file:

- Windows: `%APPDATA%\nx-azure-cache\config.toml`;
- Linux: `$XDG_CONFIG_HOME/nx-azure-cache/config.toml` (`~/.config` by default).

Each key (`port`, `account`, `container`, `credential`, `tenant_id`, `client_id`,
`managed_client_id`, `token_store`, `idle_minutes`) can be overridden by `NX_AZURE_CACHE_<KEY>`, which takes
precedence over the file:

```sh
NX_AZURE_CACHE_PORT=7485 nx-azure-cache serve
```

### Identities and config

`account` is required: without it, the Gateway runs but `/health` reports it unusable
(`ready=false`, "account missing from the config"). The `credential` key picks the Identity on
whose behalf it accesses the Blob (scope `https://storage.azure.com/.default`):

| `credential` | Flow | Inputs |
|---|---|---|
| `pipelines` | Azure Pipelines service connection OIDC | `SYSTEM_OIDCREQUESTURI`, `SYSTEM_ACCESSTOKEN`, `AZURESUBSCRIPTION_TENANT_ID`, `AZURESUBSCRIPTION_CLIENT_ID`, `AZURESUBSCRIPTION_SERVICE_CONNECTION_ID` |
| `github` | GitHub Actions OIDC, see [GitHub Actions](#github-actions) | `ACTIONS_ID_TOKEN_REQUEST_URL`, `ACTIONS_ID_TOKEN_REQUEST_TOKEN` (job with `permissions: id-token: write`), `AZURE_CLIENT_ID`, `AZURE_TENANT_ID` |
| `workload` | federated token in a file | `AZURE_FEDERATED_TOKEN_FILE`, `AZURE_CLIENT_ID`, `AZURE_TENANT_ID` |
| `managed` | IMDS | `managed_client_id` if the machine carries several managed identities |
| `cli` | `az account get-access-token` | an active `az login`; never in CI |
| `user` | developer `login`, see [Developer machine: login](#developer-machine-login) | `client_id`, `tenant_id`, `token_store` |
| `auto` (default) | a single one of the Identities above, chosen at startup | see below |

`auto` chooses deterministically, without trying several Identities one after the other:
`pipelines` if `SYSTEM_OIDCREQUESTURI` is set, otherwise `github` if
`ACTIONS_ID_TOKEN_REQUEST_URL` is, otherwise `workload` if
`AZURE_FEDERATED_TOKEN_FILE` is, otherwise `user` if a `login` stored a refresh token, otherwise
none. `managed` and `cli` are never chosen by `auto` (an IMDS probe is slow outside Azure): they
must be requested.

- **Token**: obtained at startup, cached, renewed 4 min before it expires. A failure is retried
  every 30 s; an acquisition without an answer is abandoned after 60 s. With `pipelines` and
  `github`, each renewal requests a new OIDC token (it only lives a few minutes).
- **`/health.identity`**: `{"ready": bool, "kind": "pipelines"|…|null, "reason": "…"}`, read
  without any network call. While `ready` is false, the Activation plugin does not set the URL
  and prints the reason; it is also written to the log.
- **Pitfalls reported in the reason**: `SYSTEM_ACCESSTOKEN` missing or rejected (map it with
  `SYSTEM_ACCESSTOKEN: $(System.AccessToken)` in the step's `env:`); `AZURESUBSCRIPTION_*`
  missing (start `serve` from an `AzureCLI@2` step); AADSTS700016 or AADSTS900023 (client id or
  tenant of another identity than the one holding the federated credential); AADSTS700213 or
  AADSTS70021 (no federated credential matches the token's issuer and subject);
  `ACTIONS_ID_TOKEN_REQUEST_URL` missing with `credential = github` (job without
  `permissions: id-token: write`); IMDS rejecting the identity without `managed_client_id`
  (several user-assigned managed identities).

#### GitHub Actions

The `github` Identity needs no secret. The job asks GitHub for an OIDC token and exchanges it
with Entra for a Blob token:

- the job (or workflow) grants `permissions: id-token: write`, which makes GitHub set
  `ACTIONS_ID_TOKEN_REQUEST_URL` and `ACTIONS_ID_TOKEN_REQUEST_TOKEN`;
- the step that starts `serve` sets `AZURE_CLIENT_ID` and `AZURE_TENANT_ID` to the writer
  identity (app registration or user-assigned managed identity), as `azure/login` users already do;
- that identity carries a federated credential with issuer
  `https://token.actions.githubusercontent.com`, audience `api://AzureADTokenExchange`, and a
  subject matching the job: `repo:<owner>/<repo>:ref:refs/heads/<branch>`,
  `repo:<owner>/<repo>:pull_request` or `repo:<owner>/<repo>:environment:<name>`.

```yaml
permissions:
  id-token: write
  contents: read
jobs:
  build:
    runs-on: ubuntu-latest
    env:
      AZURE_CLIENT_ID: ${{ vars.AZURE_CLIENT_ID }}
      AZURE_TENANT_ID: ${{ vars.AZURE_TENANT_ID }}
      NX_AZURE_CACHE_ACCOUNT: <account>
```

Local try with `az login`:

```sh
NX_AZURE_CACHE_ACCOUNT=<account> NX_AZURE_CACHE_CREDENTIAL=cli nx-azure-cache serve
```

### Developer machine: login

```sh
nx-azure-cache login            # browser (PKCE, redirect to http://127.0.0.1:<port>)
nx-azure-cache login --device   # no browser (SSH, container): code to enter elsewhere
nx-azure-cache whoami           # UPN and tenant, or "not signed in" (exit code 1)
nx-azure-cache logout
```

- `login` requires `tenant_id` and `client_id` (the public app registration for developer
  flows, created by `scripts/deploy-azure.ps1`, see [`docs/runbook-azure.md`](docs/runbook-azure.md)).
  There is no default: without them, `login` fails and names the missing keys.
- Only the refresh token is kept, in the system keyring (`token_store = "keyring"`, default:
  Credential Manager, Secret Service). Without a keyring (Linux without Secret Service,
  SSH session), `login` fails and points to `token_store = "file"`: the refresh
  token is then written in clear text to `refresh_token`, next to `config.toml`, mode 0600 on
  Linux, atomically. `logout` clears both locations.
- A running Gateway picks up the new Identity without restarting: `login` and `logout` send it
  `POST /reload` (Local token required). The write state goes back to `unknown`.
- Refresh token expired or revoked: `/health` switches to `ready=false`, reason "sign-in
  expired: run `nx-azure-cache login`".
- `whoami` performs a refresh (hence a refresh token rotation) to read the id token.

### Local token

On first start, the Gateway creates `local-token` next to `config.toml` (64 hexadecimal
characters, mode 0600 on Linux) and keeps it afterwards. Cache requests must carry it as
`Authorization: Bearer <token>`; without it, they get `404` (GET) or `403` (PUT).

### Reading and writing the Blob

`/{workspace}/v1/cache/{hash}` reads and writes the blob `{workspace}/{hash}` in the `container`
container of the `account` storage account. Nx only ever gets `200`, `404`, `403` or `409`:

- **GET**: streamed. Missing blob, Azure error or nothing received within 10 s: `404`. A cut in
  the middle of the stream (or 30 s without a byte) is resumed with `Range` + `If-Match` on the
  ETag, up to 3 times (stopping at the first one if the ETag changed: the blob was rewritten);
  beyond that, the connection is closed and Nx sees an error (known limitation).
- **PUT**: the body is always read to the end, and sent in 4 MiB blocks as it arrives (at most 4
  blocks in flight, as many in memory per request), then committed with `If-None-Match: *`.
  Success: `200`; Entry already present: `409`; any error: `403`.
- **Write state** (`/health.write`): `unknown` at startup, `allowed` after a successful write,
  `denied` after an Azure authorization refusal (`AuthorizationPermissionMismatch`,
  `AuthorizationFailure`). From then on, PUTs get `403` without calling Azure, until the next
  successful token renewal (which resets `denied` to `unknown`: a transient refusal, RBAC
  propagation or firewall, does not last), a restart or an Identity change (`login`, `logout`).
  A network or token error does not change the state.
- At most 16 concurrent Azure calls; extra requests wait, the wait counting towards the call's
  timeout (10 s before the first byte of a GET, 30 s per write call): beyond that, `404` or
  `403`.

## Enable the cache in a Workspace

The public npm package `nx-azure-cache` carries the Activation plugin, which connects Nx to the
machine's Gateway, and an `nx-azure-cache` command. The Gateway binary comes from a Platform
package, `@nx-azure-cache/win32-x64` or `@nx-azure-cache/linux-x64` (static musl), installed as
an optional dependency only on its platform, without any install script. Other platforms (macOS,
arm64) get no binary: the plugin sets nothing and says so. No authentication is needed to
install. The plugin is tested (e2e) on Nx 22.4.1, 23.2.1 and 24.0.5, but Nx 23.0.2 is the
recommended minimum: earlier versions have a zip-slip flaw when extracting Entries.

1. Add the package to the Workspace's dev dependencies (npm, pnpm and Bun work as is):

   ```sh
   pnpm add -D nx-azure-cache
   ```

2. Declare the plugin in `nx.json` by its package name, with the Workspace identifier
   (`^[a-z0-9][a-z0-9-]{0,62}$`) and the settings of the Gateway it starts:

   ```json
   {
     "plugins": [
       {
         "plugin": "nx-azure-cache",
         "options": {
           "workspace": "my-workspace",
           "account": "mystorageaccount",
           "tenantId": "00000000-0000-0000-0000-000000000000",
           "clientId": "00000000-0000-0000-0000-000000000000"
         }
       }
     ]
   }
   ```

Before each run, the plugin queries `http://127.0.0.1:<port>/health` (200 ms at most).

- **No Gateway**: it starts one (`serve`), detached and without a window, like the Nx daemon:
  the Gateway outlives the run and serves the next ones, from any Workspace. `account`,
  `tenantId` and `clientId` are passed to it as `NX_AZURE_CACHE_ACCOUNT`,
  `NX_AZURE_CACHE_TENANT_ID` and `NX_AZURE_CACHE_CLIENT_ID`. The run waits 5 s at most; if the
  Identity is not ready by then, it goes on without the remote cache.
- **Gateway older than the plugin**: it asks it to stop (`POST /shutdown`, Local token), then
  starts its own. A newer or equal Gateway is used as is.
- **Binary**: `NX_AZURE_CACHE_BIN`, otherwise the Platform package
  `@nx-azure-cache/<platform>-<arch>` resolved next to the plugin.
- **CI** (`CI` set): it starts nothing; the [CI start script](#ci) starts the Gateway.
- A Gateway started by another Workspace keeps that Workspace's settings: one storage account
  per machine is assumed.

If the Gateway is healthy, it sets `NX_SELF_HOSTED_REMOTE_CACHE_SERVER`
(`http://127.0.0.1:<port>/<workspace>`) and `NX_SELF_HOSTED_REMOTE_CACHE_ACCESS_TOKEN` (the
Local token). Otherwise it sets nothing, prints a line `[nx-azure-cache] remote cache off:
<reason>` and Nx runs on its local cache. It never fails a run.

- Port: `options.port`, otherwise `NX_AZURE_CACHE_PORT`, otherwise `7484`.
- `NX_AZURE_CACHE_DISABLED=1` disables the plugin for a run.
- An already defined `NX_SELF_HOSTED_REMOTE_CACHE_SERVER` is respected: the plugin does nothing.
- With the Nx daemon, the plugin's line goes to `.nx/workspace-data/d/daemon.log`, not to the
  terminal. To see it: `NX_DAEMON=false nx run <project>:<target>`.
- Nx 23 shares its local cache across all checkouts of the same git repository, worktrees
  included (`~/.nx/<id>/cache`). A fresh clone can therefore still find a local hit: to check a
  remote hit, also clear that cache (or set `"cacheDirectory": ".nx/cache"`), then `nx reset`
  (`--only-cache` is not enough: Nx's database still reports the hit).

The package's command runs the Gateway binary from the Workspace, for example
`pnpm exec nx-azure-cache login`, `status` or `version`. It reads `account`, `tenantId` and
`clientId` from the `nx-azure-cache` entry of the nearest `nx.json` and passes them as the same
`NX_AZURE_CACHE_*` variables; variables already set win. Without a readable `nx.json`, the binary
runs with the environment unchanged. Arguments, output and exit code pass through.

Plugin tests (unit, command, version script, then e2e on one workspace per tested Nx version,
`nx-plugin/e2e/nx22`, `nx23` and `nx24`, each declaring the plugin by its package name, with and
without the daemon):

```sh
pnpm test:plugin
```

### CI

In CI the plugin starts nothing: two scripts shipped in the package start the Gateway on behalf
of the CI's Identity and print its report at the end of the job. They run the Gateway of the
package version in the Workspace's lockfile. **Linux x64 agents only**, after `pnpm install`.
Copy one of the snippets below once into the pipeline.

Azure Pipelines (`pipelines` Identity):

```yaml
pool:
  vmImage: ubuntu-latest

steps:
  - checkout: self
  - script: pnpm install --frozen-lockfile
  # AzureCLI@2 references the service connection and sets the AZURESUBSCRIPTION_* variables the
  # Gateway reads. It removes them at the end of the step; the Gateway keeps its copy.
  - task: AzureCLI@2
    displayName: 'nx-azure-cache: start the Gateway'
    continueOnError: true # Safe fallback: a broken service connection does not fail the job
    inputs:
      azureSubscription: <service connection>
      scriptType: bash
      scriptLocation: inlineScript
      inlineScript: bash node_modules/nx-azure-cache/ci/start.sh
    env:
      NX_AZURE_CACHE_ACCOUNT: <account>
      SYSTEM_ACCESSTOKEN: $(System.AccessToken) # secret: never exported without this mapping
  - script: pnpm exec nx affected -t lint test build
  - script: bash node_modules/nx-azure-cache/ci/report.sh
    displayName: 'nx-azure-cache: report'
    condition: always()
```

GitHub Actions (`github` Identity, see [GitHub Actions](#github-actions) for the federated
credential):

```yaml
permissions:
  id-token: write
  contents: read
jobs:
  build:
    runs-on: ubuntu-latest
    env:
      AZURE_CLIENT_ID: ${{ vars.AZURE_CLIENT_ID }}
      AZURE_TENANT_ID: ${{ vars.AZURE_TENANT_ID }}
      NX_AZURE_CACHE_ACCOUNT: <account>
    steps:
      - uses: actions/checkout@v4
      - uses: pnpm/action-setup@v4
      - run: pnpm install --frozen-lockfile
      - name: 'nx-azure-cache: start the Gateway'
        run: bash node_modules/nx-azure-cache/ci/start.sh
        continue-on-error: true # Safe fallback
      - run: pnpm exec nx affected -t lint test build
      - name: 'nx-azure-cache: report'
        if: always()
        run: bash node_modules/nx-azure-cache/ci/report.sh
```

Prerequisites:

- Azure Pipelines: an Azure Resource Manager **service connection** with **workload identity
  federation**, created with the **Entra issuer** (the `vstoken.dev.azure.com` issuer is retired
  on 2027-07-01); Azure CLI on the agent (present on hosted agents).
- GitHub Actions: `permissions: id-token: write`, and `AZURE_CLIENT_ID` / `AZURE_TENANT_ID` of
  an identity whose federated credential matches the job.
- The identity holds a **write role on the container** (Storage Blob Data Contributor at
  container scope; see [`docs/runbook-azure.md`](docs/runbook-azure.md)).
- The Workspace declares the Activation plugin ([above](#enable-the-cache-in-a-workspace)): it
  is what connects Nx to the Gateway.

What `start.sh` does:

- Finds the Gateway binary of the installed Platform package, like the plugin does
  (`NX_AZURE_CACHE_BIN` wins), and selects the CI's Identity (`pipelines` or `github`) unless
  `NX_AZURE_CACHE_CREDENTIAL` is set.
- If a Gateway already answers on the port (host shared by several agents: it belongs to another
  job), it does not adopt it: a warning, `NX_AZURE_CACHE_DISABLED=1` set for the next steps, and
  the job runs on its local cache.
- Starts `serve` in the background, stdout and stderr redirected to `nx-azure-cache.log` in the
  agent's temp directory (`Agent.TempDirectory`, `RUNNER_TEMP`). It serves the following steps;
  the agent kills it at the end of the job.
- If the Gateway is not ready after 60 s, or anything else fails: a warning, the reason
  (`status`), exit code 0, and the job goes on with the local cache (**Safe fallback**).
- No secret is written to the job log. The Gateway process keeps `SYSTEM_ACCESSTOKEN` (Azure
  Pipelines) or `ACTIONS_ID_TOKEN_REQUEST_TOKEN` (GitHub Actions) in its environment, readable
  by the agent's user: harmless on a hosted agent (disposable VM), to keep in mind on a shared
  agent.

`report.sh` prints `status` (health, Identity, write state, per-Workspace stats) and the last 50
lines of the log, excluding the `/health` and `/stats` probes. It never fails.

**Release pipelines do not start the Gateway**: a `release` never reads the cache, so it never
ships a `dist` coming from the cache ([ADR 0001](docs/adr/0001-trust-model.md)).

Local test of both scripts, outside any CI, as Azure Pipelines then as GitHub Actions (Docker;
a Gateway that never becomes ready, then a second start that finds it already there):

```sh
pnpm build:linux && bash ci/test-start.sh
```

## Troubleshooting

```sh
nx-azure-cache status          # readable: version, health, Identity, write state, stats
nx-azure-cache status --json   # {"health": …, "stats": …}, for scripts
```

`status` reads the port from the config, queries `/health`, then `/stats` with the Local token
(`local-token`, read from disk). If nothing answers, it prints "Gateway stopped" and exits with
code 1.

`GET /stats` requires the Local token (`401` otherwise) and returns, under `workspaces`, the
per-Workspace counters since startup: `hits`, `misses`, `writes`, `conflicts` (`409`),
`forbidden` (`403`, any reason), `azure_errors`, `bytes_read`, `bytes_written`. The `identity`
field holds the UPN for the `user` Identity once the first token is obtained, otherwise the name
of the selected Identity (`kind`, as in `/health`), `null` if none.

```sh
curl -H "Authorization: Bearer $(cat ~/.config/nx-azure-cache/local-token)" http://127.0.0.1:7484/stats
```

Log: one timestamped line (UTC) per request: Workspace (`workspace=`), first 12 characters of
the hash, method, outcome (`hit`, `miss`, `stored`, `exists`, `denied`, `error`), bytes,
duration, status returned to Nx, reason or Azure code. It goes to stdout and to a daily file,
kept 7 days:

- Windows: `%LOCALAPPDATA%\nx-azure-cache\logs\nx-azure-cache.YYYY-MM-DD.log`;
- Linux: `$XDG_DATA_HOME/nx-azure-cache/logs/` (`~/.local/share` by default).

The file's day is the UTC day. The log contains no secret: no token, no header, no query string.

## Stopping and removing

Nothing is installed: the Activation plugin starts the Gateway when needed, and the Gateway exits
after `idle_minutes` (180 by default, `0` = never) without a request. To stop it now:

```sh
curl -X POST -H "Authorization: Bearer $(cat ~/.config/nx-azure-cache/local-token)" http://127.0.0.1:7484/shutdown
```

On Windows, the token is in `%APPDATA%\nx-azure-cache\local-token`. `POST /shutdown` answers
`401` without the Local token.

To remove every trace from a machine: `nx-azure-cache logout` (refresh token, keyring or file),
then delete the config directory (`config.toml`, `local-token`) and the log directory (see
[Config](#config) and [Troubleshooting](#troubleshooting)).

## Releasing

One version for the Gateway and the three npm packages ([ADR 0006](docs/adr/0006-npm-publication-from-ci-approved-by-hand.md)),
with `Cargo.toml` as the source:

```sh
pnpm version:set 1.2.3   # writes Cargo.toml, Cargo.lock and the three npm/*/package.json
```

Commit that change alone and merge it through a pull request, then tag the merge commit on
`main` and push the tag:

```sh
git tag v1.2.3 origin/main
git push origin v1.2.3
```

The `Publish` workflow runs the CI checks, refuses a tag that disagrees with the files, builds
the Windows and static Linux binaries, then stages the three packages on npm through trusted
publishing (no token). Each staged version stays private until the maintainer approves it on
npmjs.com with 2FA; approve the Platform packages before `nx-azure-cache`.

## License

[MIT](LICENSE).
