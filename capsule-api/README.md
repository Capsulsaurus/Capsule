# Capsule API

This is API service for all Capsule clients, written in Rust.

There are multiple servable components (built together for development but separately for production):

- [`auth`](auth/README.md): Federated authentication and user management (REST)
- [`media`](media/README.md): High-performance media serving (REST)
- [`upload`](upload/README.md): High-performance, resumable upload server (REST+TUS)
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

### Generating API specifications

There is one API specification that describes the API:

- `openapi.json`: OpenAPI specification for REST APIs. Run `cargo run --bin gen_openapi --features=full -- ./openapi.json` to generate.

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
  - Media: <http://localhost:3000/v1/media>
  - Upload: <http://localhost:3000/v1/upload>

  - OpenAPI Docs (Scalar): <http://localhost:3000/openapi>
  - OpenAPI Docs (Swagger UI): <http://localhost:3000/swagger-ui>
  - OpenAPI JSON: <http://localhost:3000/openapi.json>

### Building with Podman

_Note: These commands usually work similarly across other OCI tools like Podman/Docker. But prefer building with containerd._

- Build local image: `podman build -t capsule-api:latest -f Containerfile .`
- Run local build: `podman run --network host --env-file ./.env capsule-api:latest`
