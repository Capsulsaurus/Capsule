---
title: Testing and Validation
description: What each layer is tested for, with which tools, and what must pass before Capsule holds real data.
status: draft
---

Capsule is trusted with originals that cannot be recreated, so correctness is proven by
automated, deterministic tests — never by a person or a model clicking through a flow. Every
layer tests what it owns and mocks what it does not. A behaviour a user can trigger must be
reachable by a test through the same code path: the CLI's `--json` subcommands and the TUI's
reducer exist partly so that the test suites drive exactly what a user drives.

## Rules

- **Deterministic.** Randomized tests are seeded and print their seed; a failure replays from it.
  No test depends on wall-clock timing, network access or test order.
- **Faults are first-class inputs.** Every transfer step is tested under drop, duplicate,
  reorder, truncation, server error, version refusal and process kill, not only on the happy path.
- **Invariants over examples.** Where a property can be stated (merge is commutative, a record
  round-trips byte-identically, a released original is always durable elsewhere), it is tested
  as a property across generated inputs.
- **One adapter contract, many adapters.** Every port has a conformance suite that every
  adapter, including the in-memory test double, must pass.
- **Adjacent, not inline.** A module's unit tests live in its sibling `tests.rs`
  (`foo.rs` → `foo/tests.rs`); `<crate>/tests/` exercises only the public API.
- **Tests guard the data, not the coverage number.** Mutation testing on the critical modules
  shows whether a test actually constrains behaviour.

## By layer

| Layer | What it must prove | Tools |
| --- | --- | --- |
| `capsule-core` | Canonical CBOR round-trips; unknown fields survive a re-sign; CRDT merge is commutative, associative and idempotent; every decoder rejects malformed input without panicking; cryptography matches known-answer vectors; a library write is durable before a source is released | unit tests, `proptest`, `cargo-fuzz` per decoder, `cargo-mutants` on CRDT, transfer and provenance code |
| `capsule-sdk` | Upload, fetch and restore state machines reach the right end state under every fault schedule; resume is driven by server truth; version refusals surface as one typed error | the real-TCP mock server plus `proptest`-generated fault schedules |
| `capsule-server` | Every adapter passes its port's conformance suite; finalize never leaves an index row without a blob; GC never collects a referenced blob; a killed server loses nothing acknowledged; migrations apply forward | conformance suites, fault injection, testcontainers (PostgreSQL, Valkey), `SIGKILL` binary tests |
| Compatibility | Older records and wire messages stay readable and are never rewritten by a component that does not understand them ([Versioning](/design/versioning/)) | golden fixtures per protocol version in `capsule-e2e/tests/fixtures/golden/`, recorded once from the real server and library and never edited after a release speaks that version: every later server answers each replayable request as recorded or refuses the first write with `426`, the generated client decodes each response with and without an unknown field, and every signed record decodes, verifies and re-emits byte for byte; after the first release, the previous release's CLI against the new server and the reverse ([#761](https://github.com/Capsulsaurus/Capsule/issues/761)) |
| `capsule-cli` | Every subcommand's `--json` output and exit code are a stable contract | `insta` snapshots, spawning the real binary through `CARGO_BIN_EXE_capsule` |
| TUI | Each action produces the right state; each state renders the right frame; no input waits more than one frame (16 ms) for a response | reducer unit tests, ratatui `TestBackend` frames snapshotted with `insta`, a scripted key-event driver |
| `capsule-e2e` | The real CLI and the real server converge after any interruption: every asset durable exactly once, restored byte-equal | real binaries over a fixture corpus, `SIGKILL` at seeded points during import, push and restore; a nightly soak over many seeds |

## The dogfood gate

Before real data goes in, the checklist in the
[MVP dogfood gate](https://github.com/Capsulsaurus/Capsule/issues/687) must pass: the
kill-anywhere suite over many seeds, a week of green nightly soaks, a byte-equal restore of the
full corpus, a backup-and-restore drill of a durable deployment, and a real Google Takeout
archive imported, pushed and restored.
