## Language

Everything published to GitHub for this project is in English: issues, comments, triage notes, agent briefs, pull requests, review comments, commit messages, labels. This holds even when the conversation with the maintainer is in another language. Repository content follows the same rule (see `CONTRIBUTING.md`).

## Agent skills

### Workflow

Every step of work in this repo runs through its `mattpocock-skills` skill, before the step starts:

- implementing a ticket: `tdd`, at seams agreed with the maintainer, then `code-review`;
- a bug: `diagnosing-bugs`;
- a pull request body: `pr`;
- a plan or ticket still open to question: ask the maintainer to run `/mattpocock-skills:grill-with-docs` (manual-only, like `implement`).

### Issue tracker

Issues live in the GitHub Issues of `stevebeauge/nx-azure-cache` (`gh` CLI). See `docs/agents/issue-tracker.md`.

### Triage labels

The five canonical labels, under their default names. See `docs/agents/triage-labels.md`.

### Domain docs

A single context: `GLOSSARY.md` and `docs/adr/` at the root. See `docs/agents/domain.md`.
