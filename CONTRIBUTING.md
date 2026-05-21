# Contributing to Capsule

First off, thank you for considering contributing! It’s people like you who make this project a great tool for everyone.

## Contribution Workflow

1. **Fork & Branch:** Create a branch for your work (e.g., `feat/add-zod-schemas` or `fix/issue-123`).
2. **Atomic Commits:** Keep commits small and focused. One commit should equal one logical change.
3. **Tests:** Ensure all existing tests pass and add new ones for any new features or bug fixes.
4. **Pull Request:** Open a PR against the `master` branch. Clearly describe *what* changed and *why*. You may use available PR templates as necessary. Keeping code changes within reasonable size will help us get it reviewed better and merged sooner.

## Baseline for Ownership & Provenance

To maintain high security and legal standards, we require all merge commits to be signed at minimum. However, it is still strongly recommend that you sign all your Git commits if you don't already (takes 5 minutes to setup)!

While we will still accept PRs with unsigned commits, to maintain transparent ownership, we strictly use merge (no squash nor rebasing).

## Coding Standards

* Linting: If you have LSPs configured in your editor for the various languages in your repo, it should lint using the appropriate tool with correct versions by default.
* Commit messaging: Please use **Semantic Commits** (e.g., `feat:`, `fix:`, `docs:`, `test:`) to help us automate our changelog.
* Development Patterns: Refer to [Development](/capsule-docs/src/content/docs/development/).
* AI usage: Refer to [AI.md](./AI.md).

### Contributor Checklist

- [ ] I have signed the [Contributor License Agreement](CLA.md).
- [ ] My code follows the project's style guidelines.
