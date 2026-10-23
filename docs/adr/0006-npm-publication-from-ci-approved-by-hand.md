# Every npm version is built by CI and approved by hand

The npm packages run an executable on every machine that installs them, so no long-lived credential may publish them. Every real version is published by the GitHub Actions workflow through npm trusted publishing (OIDC, no token in a secret), and only as a **staged** version: it stays private until the maintainer approves it on npmjs.com with 2FA. A compromised workflow or GitHub account alone cannot release a binary.

- **Reserved names.** Trusted publishing can only be configured on a package that already exists. The maintainer publishes, once and by hand with 2FA, an empty `0.0.0` of `nx-azure-cache`, `nx-azure-cache-win32-x64` and `nx-azure-cache-linux-x64` (a README pointing to the repository), then configures each package to trust the publish workflow with stage publishing only, and disallows tokens. Reserving the **Platform package** names early also prevents someone else from taking one that the main package would install.
- **One version, written in the commit.** `Cargo.toml` is the source. A version bump is one commit, made by a script that writes the same version to `Cargo.toml` and the three `package.json`. The plugin reads its version from its own `package.json`. The workflow refuses to publish when the `vX.Y.Z` tag, `Cargo.toml` and the `package.json` disagree, so every published version matches a state of the repository.
- **Two platforms.** `win32-x64` and `linux-x64` (static musl). Elsewhere, no binary is installed: the plugin sets nothing (**Safe fallback**) and says once that its platform has no binary. A new platform is a new **Platform package**, without breaking change.
- **CI before publication.** Tests run on every pull request and on `main`, Windows and Linux. The publish workflow starts from a `v*` tag.

Options ruled out:

| Option | Why it is ruled out |
|---|---|
| npm token in a GitHub secret | A leaked secret publishes a trapped binary to every user |
| First real version published by hand from CI-built binaries | That version has no provenance and depends on a manual gesture |
| Publishing directly on a `v*` tag, without approval | A tag push, from a compromised account or workflow, is enough to release |
| Versions rewritten by the workflow from the tag | The published package matches no commit |

Decided on 2026-10-23.
