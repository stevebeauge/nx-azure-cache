# Gateway in Rust, Nx plugin in JS, no monorepo tool

The **Gateway** is written in Rust, while the rest of the ecosystem (Nx, the **Activation plugin**) is JS. The reason is delivery: the gateway is a machine prerequisite that runs unattended on developer machines and in every CI job. Rust ships one static binary per OS, without a runtime. The Azure SDK for Rust 1.x covers CI (pipeline OIDC, workload identity, managed identity, `az`) and conditional Blob operations. The developer machine flows (browser PKCE, device code, refresh, keyring) are **copied** from an existing internal tool: about 470 lines, rather than a shared crate that would couple both repositories and drag in that tool's domain logic.

Node as a SEA executable was ruled out. All the auth is available there, but the packaging is not:
- SEA is not stable;
- the native modules `keytar` (archived) and `dpapi` require custom bundling;
- a patch of `@azure/msal-node-extensions` has to be maintained;
- libsecret is mandatory on Linux;
- the binary weighs 93 to 124 MiB, unsigned, and depends on glibc.

Reusing the Workspaces' `node` was also ruled out: each Workspace pins its version, and a session start does not see the `node` of nvm or fnm.

The repository is polyglot:
- the crate at the root;
- the plugin in `nx-plugin/` (one `.js`, `node:test` tests);
- the template in `ci/`;
- a root `package.json` **without dependencies** as the entry point for scripts.

No monorepo tool is installed, because the pieces share only an HTTP contract and no build order. The day one is needed, `nx init` will take over the existing scripts without migration.

Decided on 2026-10-01.
