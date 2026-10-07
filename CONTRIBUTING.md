# Contributing

The project is a work in progress: open an issue before a large change.

## Layout

- the Gateway crate at the root (`src/`, `tests/`);
- the Activation plugin in `nx-plugin/` (one dependency-free `.cjs`, `node:test` tests, e2e
  workspace in `nx-plugin/e2e/`);
- the Azure Pipelines templates in `ci/`;
- the scripts in `scripts/`, run through the root `package.json`.

Vocabulary is in [`CONTEXT.md`](CONTEXT.md), decisions in [`docs/adr/`](docs/adr/).

## Prerequisites

- stable Rust (`rustup`);
- Node.js and pnpm (use pnpm, not npm: the version is pinned in `packageManager`);
- Docker, only for the static Linux binary and the CI script test.

## Build and test

```sh
pnpm build          # Gateway, release build
pnpm test           # Gateway tests (cargo test)
pnpm test:plugin    # plugin unit tests, then e2e on an Nx workspace
pnpm build:linux    # static Linux binary in dist/ (Docker)
bash ci/test-start.sh   # CI start/report scripts, after pnpm build:linux (Docker, Python + PyYAML)
```

`pnpm test` and `pnpm test:plugin` must pass before a pull request.

## Conventions

- Everything in English: code, comments, displayed and logged messages, tests, docs.
- Use the glossary terms (Gateway, Workspace, Entry, Identity…) and the terms it avoids.
- A decision that is hard to reverse gets an ADR in `docs/adr/`, in the same format as the
  existing ones.

By contributing, you agree that your contributions are licensed under the [MIT license](LICENSE).
