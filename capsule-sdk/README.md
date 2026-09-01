# Capsule SDK

The previous Progenitor client is quarantined under `legacy-review/sdk-progenitor/` and is not a
Cargo package. The replacement SDK is generated with Spargen from the canonical Kynos
REST/OpenAPI document, then wrapped by small Capsule-owned workflow modules.

## Development

- The typed REST client is generated from the server's OpenAPI **3.2** schema by
  [`spargen`](https://crates.io/crates/spargen), our in-house generator (slice `S-D8` in the
  repo-root `SLICES.md`), pinned at `0.4.0` as a build-dependency. The previous progenitor
  pipeline required a lossy 3.1→3.0 schema down-conversion and is gone — we do not downgrade
  schemas.
- **What is generated, and what is not.** Generated: every request and response body, every
  typed parameter, and the byte-serving endpoints. The two gaps that once justified hand-written
  byte paths — binary response decoding and typed parameter serialization — closed in spargen
  0.2.2, so nothing here parses a response by hand any more. Hand-written and staying that way:
  the resumable upload state machine, token refresh, sync, and recovery. Those are orchestration
  *over* the generated calls, not a second parser.
- `AuthenticatedClient` is live: it wraps the generated `Client` and composes the token,
  protocol-version, and retry behaviour on top of it. Reach for the generated client directly
  only when you need a surface the wrapper does not cover.
- What the SDK is waiting on is the **Kynos OpenAPI contract**, not spargen. `capsule-server`
  exists and emits a pinned 3.2 document, but it serves one operation so far; the generated
  surfaces track whichever document is committed here and are regenerated as the port lands
  each surface.
- Four operations in the currently-committed document are structurally invalid and are narrowed
  out of generation (`spargen::OmitRule` in `build.rs`): `POST /v1/albums/{album_id}/ops`
  declares no responses at all, and three `/v1/auth/devices/...` routes carry a path-template
  variable with no matching path parameter. They were already uncallable from a typed client,
  which is why the directory client here is hand-written. Both classes stop being expressible
  under Kynos, where status is part of the return type and path params are checked at compile
  time.
