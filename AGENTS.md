# Capsule

## Code Style

- Self-validation: Most if not all code should be modular, reusable, and testable. The code that requires human review and manual testing should be minimal and focused on user facing features. All critical code must be primarily covered by complete and automated tests.
- Contract-driven development: Define the interfaces and data structures first, along with all test cases, before implementing the actual logic.
- Cohesion: All code should be split into cohesive modules that have a single responsibility and clear interfaces. Encapsulate unnecessary details.
- Minimalism: Choose to use a dependency if it reduces the scope of testing and quantity of code and as long as it does not compromise on performance and required capabilities.
- Traceability: all critical processes are verbosely logged so it is clear what happened after the fact and recovery can be feasible. Use INFO logs where necessary and DEBUG,TRACE aggressively for all critical processes. Logs should be structured and easily queryable. Instrument hot paths (e.g. major functions) for performance monitoring and debugging in production.
- Mocking: Use mocks for all external dependencies and critical internal processes. This allows us to have deterministic tests and easily simulate edge cases and failure scenarios that are hard to reproduce with real dependencies. Do not try to wire up two incomplete complex systems to mock each other.
- Linting and formatting is setup to be strict. Pre-commit and pre-push hooks are configured so you won't be able to push code that doesn't meet the standards.

## Invariants

- Every change must preserve the [system invariants](capsule-docs/src/content/docs/design/versioning.md#system-invariants): a version mismatch may refuse a write, never lose or corrupt data.
- Identifiers: UUIDv7 for new ids; UUIDv4 only where creation time must not leak.

## Workflow

- Work is tracked in GitHub issues; [ROADMAP.md](ROADMAP.md) is the ordered path to the dogfood release. Issues labelled `track:deferred` are not drained automatically.
- The CLI/TUI is the reference client: a feature lands there first, every user flow is reachable as a `--json` subcommand, and it is tested per the [testing plan](capsule-docs/src/content/docs/development/testing.md).

