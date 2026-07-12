# Capsule SDK

SDK for Capsule API. Assess all Capsule APIs statelessly via one library only. Note this SDK currently is for Rust and rather than supporting other languages via bindings, we recommend generating the respective OpenAPI, gRPC, etc. clients via the coresponding API specifications you need with tools from the native language.

## APIs Supported

- [Auth](../capsule-api/auth/README.md)
<!-- - [Upload](../capsule-api/upload/README.md)
- [Metadata](../capsule-api/metadata/README.md) -->

## Development

- The typed REST client is generated from the server's OpenAPI **3.1** schema by `spargen`,
  our in-house generator (in development; slice `S-D8` in the repo-root `SLICES.md`).
  The previous progenitor pipeline required a lossy 3.1→3.0 schema down-conversion and is
  gone — we do not downgrade schemas. Until spargen lands, `AuthenticatedClient` is parked
  (commented out) in `src/lib.rs` and the hand-written surfaces stand alone.

## Usage

Add this to your `Cargo.toml`:

```toml
[dependencies]
capsule-sdk = "0.1"
```
