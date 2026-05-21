# Capsule API

This is API service for all Capsule clients, written in Rust.

There are multiple servable components (built together for development but separately for production):

- [`auth`](auth/README.md): Federated authentication and user management (REST)
- [`library`](library/README.md): Client library operations - assets, albums, search (GraphQL)
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

There are three API specifications that programatically describe the API:

- `openapi.json`: OpenAPI specification for REST APIs. Run `cargo run --bin gen_openapi --features=full -- ./openapi.json` to generate.
- `schema.graphql`: GraphQL schema for library GraphQL API. Run `cargo run --bin gen_graphql_schema > schema.graphql` in [library](./library/) to generate.
- `metadata.proto`: Protocol Buffers schema for the sync gRPC API. See [sync/proto](./sync/proto/) for the definitions.

### Testing

Most tests are written to require minimal system dependencies. However, some are still required:

- Enable memory overcommit (Linux): `sudo sysctl vm.overcommit_memory=1` (or add to `/etc/sysctl.d/90-overcommit.conf`)
- 
<!-- - If using Podman (i.e. not Docker), testcontainers requires a Docker-compatible socket:
  - Enable socket: `systemctl --user enable --now podman.socket`
  - Check status: `systemctl --user status podman.socket`
  - Configure environment variable: `export DOCKER_HOST=unix:///run/user/$UID/podman/podman.sock`
  - Disable ryuk if running Podman in rootless mode: `export TESTCONTAINERS_RYUK_DISABLED=true` -->

### Running

- Spin up some dependencies: `podman compose up` (could spin up individual services manually if needed)
  - Note for SELinux: We use `:Z,U` mount options in `compose.yaml` to ensure proper permissions.
  - Remove existing data: `podman compose down -v`
- Start development server: `RUST_BACKTRACE=1 COLORBT_SHOW_HIDDEN=1 systemfd --no-pid -s 3000 -- cargo watch -x run`
  - _Append feature flags to enable specific parts of server_
- The following endpoints should be up:
  - Auth: <http://localhost:3000/v1/auth>
  - Library (GraphQL): <http://localhost:3000/v1/library>
    - GraphiQL (debug build only): <http://localhost:3000/v1/library/playground>
  - Media: <http://localhost:3000/v1/media>
  - Upload: <http://localhost:3000/v1/upload>
  - Sync (gRPC): <http://localhost:3000/v1/sync> (requires H2C/gRPC client)

  - OpenAPI Docs (Scalar): <http://localhost:3000/openapi>
  - OpenAPI Docs (Swagger UI): <http://localhost:3000/swagger-ui>
  - OpenAPI JSON: <http://localhost:3000/openapi.json>

### Building with Podman

_Note: These commands usually work similarly across other OCI tools like Podman/Docker. But prefer building with containerd._

- Build local image: `podman build -t capsule-api:latest -f Containerfile .`
- Run local build: `podman run --network host --env-file ./.env capsule-api:latest`
