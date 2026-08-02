# Capsule SDK

SDK for Capsule API. Assess all Capsule APIs statelessly via one library only. Note this SDK currently is for Rust and rather than supporting other languages via bindings, we recommend generating the respective OpenAPI, GraphQL, gRPC, etc. clients via the coresponding API specifications you need with tools from the native language.

## APIs Supported

- [Auth](../capsule-api/auth/README.md)
<!-- - [GraphQL](../capsule-api/graphql/README.md)
- [Upload](../capsule-api/upload/README.md)
- [Metadata](../capsule-api/metadata/README.md) -->

## Development

- Generate OpenAPI spec:
  - Start up external dependencies with `podman compose up` in [capsule-api](../capsule-api).
  - Run `./generate_openapi.sh` in [capsule-sdk](./).
  - Note: [Progenitor](https://github.com/oxidecomputer/progenitor) is used to generate the SDK as long as the OpenAPI spec is provided at the assumed path.

## Usage

Add this to your `Cargo.toml`:

```toml
[dependencies]
capsule-sdk = "0.1"
```
