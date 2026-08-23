//! `GET /v1/version` — who this server is and what build it is running.
//!
//! The first operation ported to Kynos, chosen because it is the one surface with no
//! authentication, no database and no key material, so it exercises the framework seam and
//! nothing else. `capsule status` reads it to decide whether the endpoint is reachable
//! (`capsule-cli/src/status.rs`), so the shape is a live contract, not a placeholder.

use kynos::prelude::*;
use serde::{Deserialize, Serialize};

/// Identifies the running server.
///
/// Deliberately incurious: a name and a version, no build host, no commit, no uptime, no
/// feature list. This endpoint is unauthenticated, so everything it returns is public, and a
/// key-free server has no reason to hand an anonymous caller a fingerprint of its deployment.
/// Exact client build identification runs the other way (`S-D15`) — clients tell the server
/// what they are, not the reverse.
#[derive(Schema, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct VersionResponse {
    /// The server package name.
    pub name: String,
    /// The server package version.
    pub version: String,
}

/// Reports the server's name and version.
///
/// Unauthenticated and side-effect free. Clients use it as a reachability probe before
/// attempting a protocol handshake, so it must stay cheap and must never fail for a reason
/// the caller could act on — there is no failure variant, and the return type says so.
#[kynos::get("/v1/version")]
pub async fn get_version() -> Json<VersionResponse> {
    Json(VersionResponse {
        // The Salvo server reported its own crate's name and version here, and the wire
        // contract is `{"name":"capsule-api","version":"0.1.0"}`. Keeping the literal rather
        // than `env!("CARGO_PKG_NAME")` means the rebuild does not silently rename the server
        // out from under every client that probes it; the rename is its own decision.
        name: "capsule-api".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
    })
}
