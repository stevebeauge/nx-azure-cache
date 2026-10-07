# Local Gateway rather than a central server or an existing solution

The Nx remote cache is served by a **Gateway** local to each machine. It listens on `127.0.0.1`, speaks the Nx self-hosted cache protocol and reads/writes Azure Blob directly with the machine's Entra **Identity**.

The need comes from the CI of a large consuming Workspace: 28 min median per run, ~4 runs per PR (measured in September 2026). Once the cache-free levers are applied, what is left for the remote cache is the successive pushes of the same PR and comfort on developer machines. With `nx affected`, the gain comes from the cache the PR writes itself and reads back on its next push.

Options ruled out (findings from September 2026):

| Option | Why it is ruled out |
|---|---|
| `@nx/azure-cache` (official) | Deprecated since 2026-05-21 (CREEP, CVE-2025-36852), no more fixes; peers `nx < 23`; EULA license with an activation key |
| Custom runner (`tasksRunnerOptions`) | Removed: since Nx 22.4.1, only the default runner and `nx-cloud` are recognized |
| Nx Cloud | Build data hosted by Nx, while everything must stay in-house; static CI token |
| Azure Pipelines native cache (`Cache@2`) | CI only, while developer machines are in scope. Nx 22 also rejects a `.nx/cache` restored from another machine (database named after `MachineGuid`) |
| Central server (Function or App Service + Blob) | Hosting to keep alive, inbound auth to validate, 210 MB max per request on Functions (some SPFx outputs reach 673 MB), and the Nx client fails the run if the server goes down |
| Community servers (`nx-cache-server` and the like) | Not audited, static inbound token, none validates an Entra token or distinguishes read from write |

What the local gateway solves at once:
- the infrastructure boils down to a storage account;
- no inbound auth to validate, since it is `localhost`;
- no secret, outbound auth goes through Entra;
- read-only developer machines are enforced by Azure RBAC;
- a Blob outage becomes a **Miss**, not a failed run;
- the data stays in the organization's own tenant.

The price: a binary to install and start on every machine, developer machine and CI agent alike.

Decided on 2026-10-01.
