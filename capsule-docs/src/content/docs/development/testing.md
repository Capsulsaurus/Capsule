---
title: Testing and Validation
description: What each layer is tested for, with which tools, and what must pass before Capsule holds real data.
status: draft
---

Capsule is trusted with originals that cannot be recreated, so correctness is proven by automated,
deterministic tests, never by a person or a model clicking through a flow. Every layer tests what it
owns and mocks what it does not. A behaviour a user can trigger must be reachable by a test through
the same code path. The CLI's `--json` subcommands and the TUI's reducer exist partly so the test
suites drive exactly what a user drives.

## Rules

- **Deterministic.** Randomized tests are seeded and print their seed, and a failure replays from
  it. No test depends on wall-clock timing, network access or test order.
- **Faults are first-class inputs.** Every transfer step is tested under drop, duplicate, reorder,
  truncation, server error, version refusal and process kill, not only on the happy path.
- **Invariants over examples.** Where a property can be stated (merge is commutative, a record
  round-trips byte-identically, a released original is always durable elsewhere), it is tested as a
  property across generated inputs. Property runs are seeded and print their seed, and
  `PROPTEST_RNG_SEED` replays one.
- **One adapter contract, many adapters.** Every port has a conformance suite that every adapter,
  including the in-memory test double, must pass.
- **Adjacent, not inline.** A module's unit tests live in its sibling `tests.rs` (`foo.rs` to
  `foo/tests.rs`), and `<crate>/tests/` exercises only the public API. `mise run test-layout-check`,
  part of `check-rust`, rejects any inline module under a crate's `src/` that is named `tests` or
  gated on `cfg(test)`.
- **Tests guard the data, not the coverage number.** Mutation testing on the critical modules shows
  whether a test actually constrains behaviour.

## By layer

| Layer | What it must prove | Tools |
| --- | --- | --- |
| `capsule-core` | Canonical CBOR round-trips; unknown fields survive a re-sign; CRDT merge is commutative, associative and idempotent; every decoder rejects malformed input without panicking; cryptography matches known-answer vectors; a library write is durable before a source is released | Unit tests, `proptest`, `cargo-fuzz` per decoder, `cargo-mutants` on CRDT, transfer and provenance code |
| `capsule-sdk` | Upload, fetch and restore state machines reach the right end state under every fault schedule; resume is driven by server truth; version refusals surface as one typed error | The real-TCP mock server plus `proptest`-generated fault schedules |
| `capsule-server` | Every adapter passes its port's conformance suite; finalize never leaves an index row without a blob; GC never collects a referenced blob; a killed server loses nothing acknowledged; migrations apply forward | Conformance suites, fault injection, testcontainers (PostgreSQL), `SIGKILL` binary tests |
| Compatibility | Older records and wire messages stay readable and are never rewritten by a component that does not understand them ([](/design/versioning/)) | Golden fixtures per protocol version and cross-release CLI runs ([](#compatibility-fixtures) below) |
| `capsule-cli` | Every subcommand's `--json` output and exit code are a stable contract; a camera import receives, reports and imports a card, presents the same initiator identity on every run and answers the camera's event-channel probes | `insta` snapshots, spawning the real binary through `CARGO_BIN_EXE_capsule`; a scripted PTP/IP camera on loopback built from `ptpip`'s own codecs |
| TUI | Each action produces the right state; each state renders the right frame; no input waits more than one frame (16 ms) for a response | Reducer unit tests, ratatui `TestBackend` frames snapshotted with `insta`, a scripted key-event driver |
| Localization | App layout is logical and directional icons mirror, so a right-to-left language lays out without per-language code ([](/design/i18n/#layout)) | `xtask i18n-guard` refuses physical-direction layout in the web, Swift and Compose sources against the shared allowlist; `./gradlew :android:lint` reports `RtlHardcoded`, `RtlSymmetry`, `RtlCompat` and `RtlEnabled` as errors |
| `capsule-e2e` | The real CLI and the real server converge after any interruption: every asset durable exactly once, restored byte-equal | Real binaries over a fixture corpus, `SIGKILL` at seeded points during import, push and restore; a nightly soak over many seeds |
| Federation | One server pulls another's album with a capability its member subscribed with, over the production egress client and a real TLS handshake ([](/design/federation/)) | E2E case 4 ([](#federation-case) below) |

### Compatibility fixtures

Golden fixtures per protocol version live in `capsule-e2e/tests/fixtures/golden/`. They are recorded
once from the real server and library and never edited after a release speaks that version.

- Every later server answers each replayable request as recorded, or refuses the first write with
  `426`.
- The generated client decodes each response with and without an unknown field.
- Every signed record decodes, verifies and re-emits byte for byte.
- After the first release, the previous release's CLI runs against the new server, and the reverse.

### Federation case

E2E case 4 runs two in-process federating servers with distinct pinned keys, the album's home behind
a TLS front under a private CA. `capsule-server`'s off-by-default `test-egress` feature, enabled
only from `capsule-e2e`'s dev-dependencies, routes the peer's host to that front and trusts the CA;
it changes nothing else about the client. The case runs the pull worker one round at a time rather
than waiting on its tick.

## The dogfood gate

Before real data goes in, the checklist in the
[MVP dogfood gate](https://github.com/Capsulsaurus/Capsule/issues/687) must pass:

- the kill-anywhere suite over many seeds;
- a week of green nightly soaks;
- a byte-equal restore of the full corpus;
- a backup-and-restore drill of a durable deployment;
- a real Google Takeout archive imported, pushed and restored.
