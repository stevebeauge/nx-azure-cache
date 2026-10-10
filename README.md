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
`managed_client_id`, `token_store`) can be overridden by `NX_AZURE_CACHE_<KEY>`, which takes
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
| `workload` | federated token in a file | `AZURE_FEDERATED_TOKEN_FILE`, `AZURE_CLIENT_ID`, `AZURE_TENANT_ID` |
| `managed` | IMDS | `managed_client_id` if the machine carries several managed identities |
| `cli` | `az account get-access-token` | an active `az login`; never in CI |
| `user` | developer `login`, see [Developer machine: login](#developer-machine-login) | `client_id`, `tenant_id`, `token_store` |
| `auto` (default) | a single one of the Identities above, chosen at startup | see below |

`auto` chooses deterministically, without trying several Identities one after the other:
`pipelines` if `SYSTEM_OIDCREQUESTURI` is set, otherwise `workload` if
`AZURE_FEDERATED_TOKEN_FILE` is, otherwise `user` if a `login` stored a refresh token, otherwise
none. `managed` and `cli` are never chosen by `auto` (an IMDS probe is slow outside Azure): they
must be requested.

- **Token**: obtained at startup, cached, renewed 4 min before it expires. A failure is retried
  every 30 s; an acquisition without an answer is abandoned after 60 s. With `pipelines`, each
  renewal requests a new `oidcToken` (it only lives ~10 min).
- **`/health.identity`**: `{"ready": bool, "kind": "pipelines"|…|null, "reason": "…"}`, read
  without any network call. While `ready` is false, the Activation plugin does not set the URL
  and prints the reason; it is also written to the log.
- **Pitfalls reported in the reason**: `SYSTEM_ACCESSTOKEN` missing or rejected (map it with
  `SYSTEM_ACCESSTOKEN: $(System.AccessToken)` in the step's `env:`); `AZURESUBSCRIPTION_*`
  missing (start `serve` from an `AzureCLI@2` step); AADSTS700016 or AADSTS900023 (client id or
  tenant of another identity than the one holding the federated credential); IMDS rejecting the
  identity without `managed_client_id` (several user-assigned managed identities).

Local try with `az login`:

```sh
NX_AZURE_CACHE_ACCOUNT=<account> NX_AZURE_CACHE_CREDENTIAL=cli nx-azure-cache serve
```

### Developer machine: login

```sh
nx-azure-cache login            # browser (PKCE, redirect to http://127.0.0.1:<port>)
nx-azure-cache login --device   # no browser (SSH, container): code to enter elsewhere
nx-azure-cache whoami           # UPN and tenant, or "not signed in"
nx-azure-cache logout
```

- `login` requires `tenant_id` and `client_id` (the public app registration for developer
  flows, created by `scripts/deploy-azure.ps1`, see [`docs/runbook-azure.md`](docs/runbook-azure.md)).
  There is no default: without them, `login` fails and names the missing keys.
- Only the refresh token is kept, in the system keyring (`token_store = "keyring"`, default:
  Credential Manager, Secret Service). Without a keyring (Linux without Secret Service,
  `systemd --user` service), `login` fails and points to `token_store = "file"`: the refresh
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

The Activation plugin (`nx-plugin/nx-azure-cache.cjs`) connects Nx to the machine's Gateway. It
is a dependency-free JS file, copied into each Workspace. It runs on Nx 22.4.1 as well as 23.2.1
(e2e tests), but Nx 23.0.2 is the recommended minimum: earlier versions have a zip-slip flaw when
extracting Entries.

1. Copy `nx-plugin/nx-azure-cache.cjs` into the Workspace's `tools/` and commit it, keeping the
   `.cjs` extension: renamed to `.js` in a `"type": "module"` Workspace, it would make every
   `nx` command fail. The version is in the file header.
2. Declare it in `nx.json`, with the Workspace identifier (`^[a-z0-9][a-z0-9-]{0,62}$`):

   ```json
   {
     "plugins": [
       { "plugin": "./tools/nx-azure-cache.cjs", "options": { "workspace": "my-workspace" } }
     ]
   }
   ```

Before each run, the plugin queries `http://127.0.0.1:<port>/health` (200 ms at most). If the
Gateway is healthy, it sets `NX_SELF_HOSTED_REMOTE_CACHE_SERVER`
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

Plugin tests (unit, then e2e on an Nx 23.2.1 workspace with and without the daemon):

```sh
pnpm test:plugin
```

In Azure Pipelines CI (Linux agents), the templates in [`ci/`](ci/README.md) start the Gateway on
behalf of the service connection and print its report at the end of the job.

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

## Automatic start

Copy the binary to its final location, then, from that location:

```sh
nx-azure-cache install     # sets up the automatic start and launches the Gateway
nx-azure-cache uninstall   # stops the Gateway and removes the automatic start
```

Both commands are idempotent. After copying the binary elsewhere, rerun `install` from the new
location: the path is updated and the Gateway restarted.

- **Windows**: scheduled task `nx-azure-cache`, triggered at logon of the current user, without
  administrator rights. It runs `conhost.exe --headless "<binary>" serve`: no window is opened.
- **Linux**: unit `~/.config/systemd/user/nx-azure-cache.service` (`Restart=on-failure`),
  enabled and started by `systemctl --user`. By default, it only runs during a session of the
  user; to keep it running outside a session (CI agent, remote machine):

  ```sh
  loginctl enable-linger "$USER"
  ```

## License

[MIT](LICENSE).
