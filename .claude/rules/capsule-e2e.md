---
paths:
  - "capsule-e2e/**"
---

# capsule-e2e

## Docs

Follow:

- [Testing](../../capsule-docs/src/content/docs/development/testing.md) — which cases belong at the end-to-end layer.
- [Versioning](../../capsule-docs/src/content/docs/design/versioning.md) — `tests/golden_wire.rs` and `tests/fixtures/golden/` pin the wire format; a golden change is a protocol change.
- [Self-hosting guide](../../capsule-docs/src/content/docs/guides/self-hosting.md) — `tests/backup_restore_drill.rs` exercises the documented operator procedure.

Keep current:

- [Module Map — E2E Test Surface](../../capsule-docs/src/content/docs/design/module-map.md#e2e-test-surface) — when a case is added, removed or renamed.
