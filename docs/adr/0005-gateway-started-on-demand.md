# The Activation plugin starts the Gateway on demand

On a developer machine, nothing installs the **Gateway** and nothing starts it at logon. Like the Nx daemon, the first Nx run that finds no Gateway on the port starts one, detached, from the platform binary in the Workspace's `node_modules`; later runs, from any Workspace, reuse it. This replaces the `install` / `uninstall` commands and the "Developer machine" paragraph of ADR 0004.

- **One Gateway per machine**, as before. A second `serve` on a held port exits with code 0.
- **The newer one wins.** The plugin compares its package version with the one `/health` reports. Older Gateway: the plugin asks it to stop (`POST /shutdown`, which requires the **Local token**, so neither another local user nor a web page can stop it), then starts its own. Equal or newer Gateway, or unreadable version: used as is; backward compatibility on `/health` covers a newer one.
- **Idle stop**: the Gateway exits after 3 hours without a request (`idle_minutes`, `0` = never). The next Nx run starts it again.
- **Bounded start**: the plugin waits a few seconds at most for `/health`. Beyond that, or if the **Identity** is not ready yet, the run goes on under the **Safe fallback**; the next run uses the Gateway.
- **Settings in `nx.json`**: `account`, `tenantId` and `clientId` become plugin options. The plugin passes them as `NX_AZURE_CACHE_*` variables to the Gateway it starts, and the package's `nx-azure-cache` command does the same for `login` or `status` run from the Workspace. `config.toml` remains for overrides and manual runs. Limit: a Gateway started by another Workspace keeps that Workspace's settings; one storage account per machine is assumed.
- **Not in CI**: the plugin starts nothing when `CI` is set. The CI start script of ADR 0004 is unchanged.

Why: on 2026-10-23, the `install` command planned by ADR 0004 (copy of the binary into the user profile, stop of the running process, scheduled task at logon) got the unsigned binary quarantined by Microsoft Defender as `Trojan:Win32/Bearfoos.B!ml`, a machine-learning detection, and the scheduled task removed. The copy only existed so that a running binary would not lock `node_modules`; measured the same day on Windows 11 with pnpm 11, a program running from `node_modules` blocks neither a package upgrade by `pnpm install` nor the deletion of `node_modules`.

Options ruled out:

| Option | Why it is ruled out |
|---|---|
| `install` copying the binary into the user profile, with a scheduled task or a `systemd --user` unit | Quarantined by Defender on Windows; the copy is not needed |
| Code signing of the Windows binary | Recurring cost and an identity procedure, for a detection triggered by a behaviour the tool no longer needs |
| Reporting each release to Microsoft as a false positive | Manual, repeated on every version, days of delay |
| One Gateway per Workspace, like the Nx daemon | A port per Workspace kept in a state file, and as many processes; one Gateway already serves several Workspaces |

Decided on 2026-10-23.
