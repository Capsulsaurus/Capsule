//! Capsule client SDK: the sanctioned network path for every Capsule client
//! (upload protocol, sync/download, auth flows).
//!
//! # REST client generation (spargen; slice `S-D8` in the repo-root `SLICES.md`)
//!
//! The typed REST client is generated from the server's **OpenAPI 3.1** schema by
//! `spargen`, our in-house generator (in development). The previous progenitor
//! pipeline is gone deliberately: progenitor consumes OpenAPI 3.0 only, which
//! forced a lossy 3.1→3.0 schema down-conversion — a standing source of drift and
//! failures. We do not downgrade schemas. Until spargen lands, the hand-written
//! surfaces below (the adaptive upload strategy in [`upload`]) stand alone, and
//! the generated-client wrapper is parked below.

pub mod auth;
pub mod cohort;
pub mod fetch;
pub mod net;
pub mod recovery;
pub mod staged;
pub mod sync;
pub mod upload;
pub mod verify;

/// uniffi surface over the SDK's user-flow primitives (login → upload → status →
/// sync) for Swift/Kotlin/Linux consumers (`ffi` feature, slice `S-D9`). A thin,
/// async-capable wrapper over [`auth`], [`upload`], and [`sync`]; tokens never
/// cross the boundary (the session-handle pattern). See [`ffi`].
#[cfg(feature = "ffi")]
pub mod ffi;

#[cfg(feature = "ffi")]
uniffi::setup_scaffolding!();

/// Generated gRPC client stubs for the key-free sync feed
/// (`capsule.sync.v1.SyncService`). The proto is single-sourced from the sync
/// server crate (slice `S-C2`) and compiled client-only by `build.rs`; the
/// ergonomic consumer over this stub lives in [`sync`].
pub mod proto {
    /// `capsule.sync.v1` — the key-free sync feed contract (slice `S-C2`).
    pub mod capsule {
        pub mod sync {
            pub mod v1 {
                #![allow(clippy::pedantic, unreachable_pub)]
                tonic::include_proto!("capsule.sync.v1");
            }
        }
    }
}

// ─── Parked until spargen (S-D8) ────────────────────────────────────────────
// `AuthenticatedClient` wraps the generated `Client` type, which does not exist
// without a generator. It is commented out — not deleted — because its shape
// (default bearer header, base-url/token swap, Deref to the typed client) is the
// contract S-D8 revives; S-D7 additionally gives it a token store with async
// refresh-expiry pre-flight so callers never juggle raw access tokens.
//
// /// Authenticated OpenAPI client
// pub struct AuthenticatedClient {
//     /// Base URL of the API
//     base_url: String,
//     /// Current access token
//     access_token: String,
//     /// OpenAPI client
//     client: Client,
// }
//
// impl AuthenticatedClient {
//     pub fn new(base_url: &str, access_token: &str) -> AuthenticatedClient {
//         AuthenticatedClient {
//             base_url: base_url.to_string(),
//             access_token: access_token.to_string(),
//             client: Self::get_authenticated_client(base_url, access_token),
//         }
//     }
//
//     fn get_authenticated_client(base_url: &str, access_token: &str) -> Client {
//         let authorization_header = format!("Bearer {}", access_token);
//
//         let mut headers = reqwest::header::HeaderMap::new();
//         headers.insert(
//             reqwest::header::AUTHORIZATION,
//             authorization_header.parse().unwrap(),
//         );
//
//         let client_with_custom_defaults = reqwest::ClientBuilder::new()
//             .default_headers(headers)
//             .build()
//             .unwrap();
//
//         Client::new_with_client(base_url, client_with_custom_defaults)
//     }
//
//     /// Returns same instance with new base URL
//     pub fn with_base_url(&mut self, base_url: &str) -> &mut Self {
//         self.base_url = base_url.to_string();
//         self.client = Self::get_authenticated_client(base_url, &self.access_token);
//         self
//     }
//
//     /// Returns same instance with new access token
//     pub fn with_access_token(&mut self, access_token: &str) -> &mut Self {
//         self.access_token = access_token.to_string();
//         self.client = Self::get_authenticated_client(&self.base_url, access_token);
//         self
//     }
// }
//
// impl std::ops::Deref for AuthenticatedClient {
//     type Target = Client;
//
//     fn deref(&self) -> &Self::Target {
//         &self.client
//     }
// }
