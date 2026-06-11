# Contributing to Capsule

First off, thank you for considering contributing! It’s people like you who make this project a great tool for everyone.

## Contribution Workflow

1. **Fork & Branch:** Create a branch for your work (e.g., `feat/add-zod-schemas` or `fix/issue-123`).
2. **Atomic Commits:** Keep commits small and focused. One commit should equal one logical change.
3. **Tests:** Ensure all existing tests pass and add new ones for any new features or bug fixes.
4. **Pull Request:** Open a PR against the `master` branch. Clearly describe *what* changed and *why*. You may use available PR templates as necessary. Keeping code changes within reasonable size will help us get it reviewed better and merged sooner.

## Local Setup

Tooling versions are pinned with [mise](https://mise.jdx.dev). From the repo root:

```sh
mise install        # installs just, convco, lefthook (+ language tools)
just hooks-install   # wires up the git hooks (lefthook)
```

The hooks run formatters/linters on commit and validate that every commit message is a
[Conventional Commit](https://www.conventionalcommits.org) (via `convco`) — so `convco`
must be on your `PATH` (hence `mise install`). The same check runs on `pre-push` and on
every PR in CI.

## Baseline for Ownership & Provenance

To maintain high security and legal standards, we require all merge commits to be signed at minimum. However, it is still strongly recommend that you sign all your Git commits if you don't already (takes 5 minutes to setup)!

While we will still accept PRs with unsigned commits, to maintain transparent ownership, we strictly use merge (no squash nor rebasing).

## Coding Standards

* Linting: If you have LSPs configured in your editor for the various languages in your repo, it should lint using the appropriate tool with correct versions by default.
* Commit messaging: **Semantic Commits** (e.g., `feat:`, `fix:`, `docs:`, `test:`) are **required** — they drive version bumps and the changelog, and are enforced by `convco` on commit, push, and in CI.
* Development Patterns: Refer to [Development](/capsule-docs/src/content/docs/development/).
* AI usage: Refer to [AI.md](./AI.md).

### Contributor Checklist

* [ ] I have signed the [Contributor License Agreement](CLA.md).
* [ ] My code follows the project's style guidelines.

## Releasing

Releases are automated from Conventional Commits; one version is kept in sync across every
package (`just set-version` / the `xtask` crate).

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
