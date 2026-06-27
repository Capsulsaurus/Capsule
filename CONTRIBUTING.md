# Contributing to Capsule

First off, thank you for considering contributing! It’s people like you who make this project a great tool for everyone.

## Contribution Workflow

1. **Fork & Branch:** Create a branch for your work (e.g., `feat/add-zod-schemas` or `fix/issue-123`).
2. **Atomic Commits:** Keep commits small and focused. One commit should equal one logical change.
3. **Tests:** Ensure all existing tests pass and add new ones for any new features or bug fixes.
4. **Pull Request:** Open a PR against the `master` branch. Clearly describe *what* changed and *why*. You may use available PR templates as necessary. Keeping code changes within reasonable size will help us get it reviewed better and merged sooner.

## Local Setup

Tooling versions are pinned with [mise](https://mise.jdx.dev), which is also the task
runner and (via [hk](https://hk.jdx.dev)) the git-hook manager. From the repo root:

```sh
mise trust          # trust this repo's mise config (+ capsule-swift/mise.toml)
mise install        # installs hk, convco, cargo-nextest (+ language tools)
mise run setup      # installs the web/docs/vision package dependencies
hk install          # wires up the git hooks
```

Run tasks with `mise run <task>` — `mise tasks` lists them all (plain name = auto-fix,
`-check` suffix = verify-only). The pre-commit hook auto-formats and **stages** your
changes; pre-push runs the format/lint checks plus the test suite; and `convco` validates
every commit message as a [Conventional Commit](https://www.conventionalcommits.org). The
same checks run on `pre-push` and on every PR in CI.

> **Coming from the old `just` + `lefthook` setup?** Re-run `mise install && hk install`
> (hk overwrites the stale `.git/hooks` that called lefthook). The `justfile` is gone —
> every `just <recipe>` is now `mise run <task>` with the same name.

## Baseline for Ownership & Provenance

To maintain high security and legal standards, we require all merge commits to be signed at minimum. However, it is still strongly recommend that you sign all your Git commits if you don't already (takes 5 minutes to setup)!

While we will still accept PRs with unsigned commits, to maintain transparent ownership, we strictly use merge (no squash nor rebasing).

## Coding Standards

* Linting: If you have LSPs configured in your editor for the various languages in your repo, it should lint using the appropriate tool with correct versions by default.
* Tests: Rust tests run on [nextest](https://nexte.st) (`mise run test-rust`). Note nextest does **not** run doctests — if you add one, also run `cargo test --doc`.
* Commit messaging: **Semantic Commits** (e.g., `feat:`, `fix:`, `docs:`, `test:`) are **required** — they drive version bumps and the changelog, and are enforced by `convco` on commit, push, and in CI.
* Development Patterns: Refer to [Development](/capsule-docs/src/content/docs/development/).
* AI usage: Refer to [AI.md](./AI.md).

### Contributor Checklist

* [ ] I have signed the [Contributor License Agreement](CLA.md).
* [ ] My code follows the project's style guidelines.

## Translations

You can translate Capsule **without touching application code**. All user-facing strings live
in the canonical [`locales/`](locales/README.md) catalogs (ICU MessageFormat JSON); a build step
compiles them into every platform's native format.

1. Add your locale to `locales/config.json` and copy `en.json` to `<locale>.json`, translating
   each `message` (keep the keys identical).
2. Run `mise run i18n` to regenerate the per-platform files, then `mise run i18n-check` to confirm
   there is no drift.
3. Open a PR following the workflow above.

See [`locales/README.md`](locales/README.md) for the catalog format and the
[i18n design doc](capsule-docs/src/content/docs/design/i18n.md) for the overall design. A hosted
translation UI (Weblate/Crowdin) backed by these same files is planned.

## Releasing

Releases are automated from Conventional Commits; one version is kept in sync across every
package (`mise run set-version` / the `xtask` crate).

1. **Prepare** — run the **Prepare release** workflow (`workflow_dispatch`). It computes the
   next version with `convco` (or takes an explicit one), writes it into every package,
   regenerates `CHANGELOG.md`, and opens a `release/vX.Y.Z` PR.
2. **Review** — review/edit the CHANGELOG and version bumps on that PR. The full CI gate
   runs on it ("all checks before release").
3. **Merge** — merging the PR lands `chore(release): vX.Y.Z` on `master`, which triggers
   **release.yml**: it builds the `capsule` CLI binaries (Linux/macOS, Windows best-effort)
   and publishes a GitHub Release (the tag is created as part of this).

**One-time setup:** add a repo secret `RELEASE_PAT` (a PAT with `contents` + `pull-requests`
write). It's used only to open the release PR — PRs opened with the default token don't
trigger CI, so without it the release PR's checks wouldn't run.
