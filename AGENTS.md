# Capsule

## Code Style

- Self-validation: Most if not all code should be modular, reusable, and testable. The code that requires human review and manual testing should be minimal and focused on user facing features. All critical code must be primarily covered by complete and automated tests.
- Contract-driven development: Define the interfaces and data structures first, along with all test cases, before implementing the actual logic.
- Cohesion: All code should be split into cohesive modules that have a single responsibility and clear interfaces. Encapsulate unnecessary details.
- Minimalism: Choose to use a dependency if it reduces the scope of testing and quantity of code and as long as it does not compromise on performance and required capabilities.
- Traceability: all critical processes are verbosely logged so it is clear what happened after the fact and recovery can be feasible. Use INFO logs where necessary and DEBUG,TRACE aggressively for all critical processes. Logs should be structured and easily queryable. Instrument hot paths (e.g. major functions) for performance monitoring and debugging in production.
- Mocking: Use mocks for all external dependencies and critical internal processes. This allows us to have deterministic tests and easily simulate edge cases and failure scenarios that are hard to reproduce with real dependencies. Do not try to wire up two incomplete complex systems to mock each other.
- Linting and formatting is setup to be strict. Pre-commit and pre-push hooks are configured so you won't be able to push code that doesn't meet the standards.

## Internationalization

- No hardcoded user-facing strings. Every translatable string is a key in the canonical catalogs under `locales/` (ICU MessageFormat). Add the key there, not inline in app code.
- After editing `locales/`, run `mise run i18n` to regenerate the per-platform files (Rust bundle, web JSON, Android `strings.xml`, iOS `.xcstrings`). Generated files are committed and carry a "do not edit by hand" banner; `mise run i18n-check` (part of `check-rust`) fails on drift.
- Keys use dotted namespaces (`area.subarea.name`). Server errors carry a stable `code` from the `error.*` namespace (referenced via `capsule_i18n::error_codes`); clients localize the code while the English detail message stays English.
- See the [i18n design doc](capsule-docs/src/content/docs/design/i18n.md) for the full contract and `locales/README.md` for the contributor workflow.
