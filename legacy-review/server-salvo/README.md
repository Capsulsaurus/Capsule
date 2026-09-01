# Capsule API

This is API service for all Capsule clients, written in Rust.

There are multiple servable components (built together for development but separately for production):

- [`auth`](auth/README.md): Federated authentication and user management (REST)
- [`media`](media/README.md): High-performance media serving (REST)
- [`upload`](upload/README.md): High-performance, resumable upload server (REST+TUS)
- [`sync`](sync/README.md): Bulk library sync for mobile/desktop clients (gRPC)
- **OpenAPI**: Integrated OpenAPI spec and docs (Scalar UI, Swagger UI) - enable with `openapi` feature flag

They can be packaged together or separately (recommended for production).

## Development

### Prerequisites

_We assume Linux-based systems for this service due to use of platform-specific features. There are many tools to get a Linux environment on other OSes._

- Rustup toolchain
- Populate `.env` file based on `.env.example`
- `cargo install systemfd cargo-watch`
- `cargo install sea-orm-cli`
- Podman
  - Note: Most OCI runtimes should work identically in theory but our recommended deployment methods are Kubernetes and Podman.
- Protobuf compiler

  ```bash
  # Ubuntu/Debian
  sudo apt update && sudo apt upgrade -y
  sudo apt install -y protobuf-compiler libprotobuf-dev
  ```

  ```bash
  # Arch Linux
  sudo pacman -S protobuf
  ```

  ```bash
  # macOS
  brew install protobuf
  ```

### Generating API specifications

There are two API specifications that programatically describe the API:

- `openapi.json`: OpenAPI specification for REST APIs. Run `cargo run --bin gen_openapi --features=full -- ./openapi.json` to generate.
- `metadata.proto`: Protocol Buffers schema for the sync gRPC API. See [sync/proto](./sync/proto/) for the definitions.

### Testing

Most tests are written to require minimal system dependencies. However, some are still required:

- Enable memory overcommit (Linux): `sudo sysctl vm.overcommit_memory=1` (or add to `/etc/sysctl.d/90-overcommit.conf`)
- If using Podman (i.e. not Docker), testcontainers needs a Docker-compatible socket:
  - Linux: `systemctl --user enable --now podman.socket`, then
    `export DOCKER_HOST=unix:///run/user/$UID/podman/podman.sock`
  - macOS: `podman machine start` — it forwards `/var/run/docker.sock`, so `DOCKER_HOST`
    is not needed
  - Rootless Podman: `export TESTCONTAINERS_RYUK_DISABLED=true`
  - `capsule-api/testing` shells out to the literal `docker` binary, so a `docker` shim
    must be on PATH (podman installs one). Setting `TEST_DATABASE_URL` bypasses that path
    entirely and is the better option when running many suites in parallel.

### Running

From a clean checkout, one command (slice `S-P7`):

```bash
mise run serve-api
```

It brings up Postgres and Valkey from `compose.yaml`, seeds `capsule-api/.env` if absent
(minting a `JWT_ED25519_DER` via `mise run keygen`) and tops up any required variable that
is missing, waits for both services to answer, then runs the server. Migrations apply at
startup — but **only on a debug build** (`src/main.rs` gates `Migrator::up` on
`debug_assertions`), which is why the task deliberately does not pass `--release`. A
release deployment must run the migrator itself.

Configuration, as read by `capsule-api/environment`:

| Variable | Required | Notes |
| --- | --- | --- |
| `DATABASE_URL` | yes | e.g. `postgresql://capsule:capsule@localhost:5432/capsule` |
| `VALKEY_URL` | yes | e.g. `redis://127.0.0.1:6379` |
| `JWT_ED25519_DER` | yes | base64 PKCS#8 DER Ed25519. `mise run keygen` mints one. |
| `UPLOAD_DIR` | no (`./uploads`) | **The blob backend.** Ciphertext blobs are files; there is no object store. `capsule-gc` and `capsule-scrub` read the same path. |
| `SERVER_HOST` / `SERVER_PORT` / `SERVER_DOMAIN` | no | `0.0.0.0` / `3000` / `localhost` |
| `ALLOWED_ORIGINS` | no | `["*"]` in debug, `[]` in release |
| `SYNC_CURSOR_MAC_KEY` | no | HKDF-derived from `JWT_ED25519_DER` when unset |
| `ATTESTATION_KEY_SEED` | no | HKDF-derived from `JWT_ED25519_DER` when unset; accepts 32 or 64 bytes |

Because those last two derive from the signing key, **rotating `JWT_ED25519_DER` also rotates
the sync-cursor MAC and the attestation identity** unless they are set explicitly.

Point a client at it with `export CAPSULE_ENDPOINT=http://127.0.0.1:3000`.

To run it by hand instead — note `dotenvy` searches the _current_ directory, so either `cd
capsule-api` first or export the file yourself, which is what `serve-api` does:

- Dependencies only: `podman compose up`
  - Note for SELinux: we use `:Z,U` mount options in `compose.yaml` for permissions.
  - Remove existing data: `podman compose down -v`
- Auto-reloading server: `RUST_BACKTRACE=1 systemfd --no-pid -s 3000 -- cargo watch -x run`
  - _Append feature flags to enable specific parts of server_
- The following endpoints should be up:
  - Auth: <http://localhost:3000/v1/auth>
  - Upload: <http://localhost:3000/v1/upload>
  - Blobs (content-addressed): <http://localhost:3000/v1/blob>
  - Shares: <http://localhost:3000/v1/s> · Guest drops: <http://localhost:3000/v1/u>
  - Discovery: <http://localhost:3000/.well-known/capsule/server-info>
  - Sync (gRPC): `http://localhost:3000/capsule.sync.v1.SyncService` — at the **root**, not
    under `/v1`, because tonic clients discard any path on the endpoint URI. Requires an
    H2C/gRPC client.

  - OpenAPI Docs (Scalar): <http://localhost:3000/openapi>
  - OpenAPI Docs (Swagger UI): <http://localhost:3000/swagger-ui>
  - OpenAPI JSON: <http://localhost:3000/openapi.json>

### Building with Podman

_Note: These commands usually work similarly across other OCI tools like Podman/Docker. But prefer building with containerd._

- Build local image: `podman build -t capsule-api:latest -f Containerfile .`
- Run local build: `podman run --network host --env-file ./.env capsule-api:latest`
