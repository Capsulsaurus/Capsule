//! Publishing and fetching the **signed device directory** (slice `S-P1`, over the `S-C9`
//! server surface).
//!
//! The directory is the user-IK-signed list of enrolled device signing keys that
//! [`verify_asset`] resolves a manifest's `created_by_device` against. Until a device's entry
//! is published, every asset it signs is [`RejectReason::UnknownDevice`] to every *other*
//! device — which is why first-device enrollment publishes before it uploads anything.
//!
//! Two rules shape this module:
//!
//! - **The bytes are opaque and travel verbatim.** The server projects exactly one field out
//!   of the document — `directory_version`, for the invariant-23 monotonicity check — and
//!   stores the rest byte-for-byte. Re-encoding the document here would detach it from the
//!   signature it carries, so the canonical CBOR `capsule-core` produced is what goes on the
//!   wire and what comes back off it.
//! - **A fetched directory is not trusted until it verifies.** [`DirectoryClient::fetch`]
//!   requires the pinned user identity key and refuses a document that does not verify under
//!   it, so a server cannot hand a client a directory listing devices the user never enrolled.
//!   The signature check itself is `capsule-core`'s ([`DeviceDirectory::verify`]); nothing
//!   cryptographic is re-implemented here.
//!
//! Hand-written rather than generated: the committed OpenAPI declares no request body for the
//! publish operation, so the generated client's `publish_device_directory()` takes none. The
//! wire below is the documented one (`application/cbor` in, `{directory_version}` out).
//!
//! [`verify_asset`]: capsule_core::crypto::verify_asset::verify_asset
//! [`RejectReason::UnknownDevice`]: capsule_core::crypto::verify_asset::RejectReason::UnknownDevice

use capsule_core::crypto::keys::{DeviceDirectory, HybridVerifyingKey};
use capsule_i18n::error_codes;
use serde::Deserialize;
use tracing::instrument;
use uuid::Uuid;

use crate::auth::{AuthError, Session};

/// The device-directory path, appended to the caller's API base.
const DIRECTORY_PATH: &str = "devices/directory";

/// The `application/cbor` media type the directory surface speaks in both directions.
const CBOR: &str = "application/cbor";

/// Everything the device-directory flows can fail with. Callers switch on the typed variant
/// (or its stable `error.*` code), never on a bare status.
#[derive(Debug, thiserror::Error)]
pub enum DirectoryError {
    /// The authenticated request itself failed (transport, session expiry, refresh).
    #[error(transparent)]
    Auth(#[from] AuthError),
    /// Reading the response body off the wire failed.
    #[error("reading device-directory response body failed: {0}")]
    Body(#[source] reqwest::Error),
    /// The server rejected the document as malformed (`400`).
    #[error("the server rejected the device directory as malformed")]
    Malformed,
    /// The submitted `directory_version` does not advance the stored one (`409`, invariant 23 —
    /// the anti-rollback rule). Re-fetch, re-key, and re-publish at a higher version.
    #[error("device directory version does not advance the published one")]
    VersionConflict,
    /// No directory has been published for the requested user (`404`).
    #[error("no device directory published for this user")]
    NotPublished,
    /// The directory bytes could not be (de)serialized as the canonical `DeviceDirectory`.
    #[error("device directory codec error: {0}")]
    Codec(String),
    /// A fetched directory does not verify under the pinned user identity key — a foreign or
    /// forged document. Fail-closed: it is never returned to the caller.
    #[error("the fetched device directory does not verify under the pinned user identity key")]
    UntrustedSignature,
    /// The server returned an unmodeled status.
    #[error("unexpected {status} response from the device-directory endpoint")]
    Unexpected {
        /// The HTTP status code the server returned.
        status: u16,
    },
}

impl DirectoryError {
    /// The stable `error.*` catalog code for this failure, when one applies. Clients localize
    /// the code; the English detail message stays English.
    pub fn error_code(&self) -> Option<&'static str> {
        match self {
            Self::Malformed => Some(error_codes::DIRECTORY_MALFORMED),
            Self::VersionConflict => Some(error_codes::DIRECTORY_VERSION_CONFLICT),
            _ => None,
        }
    }
}

/// The `POST` response body: the version now stored for the user.
#[derive(Debug, Deserialize)]
struct PublishDirectoryResponseWire {
    directory_version: u64,
}

/// The device-directory client. Borrows an authenticated [`Session`], so every call rides the
/// SDK's bearer/refresh machinery and no token is handled here.
#[derive(Clone)]
pub struct DirectoryClient {
    session: Session,
    base_url: String,
}

impl DirectoryClient {
    /// Build a client against the API base URL — the same base the auth session authenticates
    /// against (e.g. `https://api.example.com/v1/auth`).
    pub fn new(session: Session, api_base_url: &str) -> Self {
        Self {
            session,
            base_url: format!("{}/{DIRECTORY_PATH}", api_base_url.trim_end_matches('/')),
        }
    }

    /// Publish `directory` for the authenticated caller, returning the `directory_version` the
    /// server now stores.
    ///
    /// Idempotent only at the *same* version is **not** the contract: invariant 23 requires the
    /// version to advance, so re-publishing an unchanged document answers
    /// [`DirectoryError::VersionConflict`]. Publish when the document actually changes — a
    /// device enrolled, a device revoked.
    #[instrument(skip_all, fields(directory_version = directory.core.directory_version))]
    pub async fn publish(&self, directory: &DeviceDirectory) -> Result<u64, DirectoryError> {
        let body = capsule_core::cbor::to_canonical_vec(directory)
            .map_err(|e| DirectoryError::Codec(e.to_string()))?;
        tracing::debug!(bytes = body.len(), "publishing the signed device directory");
        let response = self
            .session
            .execute(|c| {
                c.post(&self.base_url)
                    .header(reqwest::header::CONTENT_TYPE, CBOR)
                    .body(body.clone())
            })
            .await?;

        let status = response.status();
        if !status.is_success() {
            return Err(publish_status_error(status.as_u16()));
        }
        let wire: PublishDirectoryResponseWire =
            response.json().await.map_err(DirectoryError::Body)?;
        tracing::info!(
            directory_version = wire.directory_version,
            "device directory published"
        );
        Ok(wire.directory_version)
    }

    /// Fetch `user_id`'s signed device directory and **verify it under `pinned_user_ik`** before
    /// returning it. A document that does not verify is [`DirectoryError::UntrustedSignature`]
    /// and never reaches the caller.
    ///
    /// The anti-rollback high-water mark is the caller's to keep: compare
    /// `core.directory_version` against the highest version already seen for this user and
    /// refuse a regression, exactly as the sync feed's `sync_seq` is refused.
    #[instrument(skip_all, fields(user_id = %user_id))]
    pub async fn fetch(
        &self,
        user_id: Uuid,
        pinned_user_ik: &HybridVerifyingKey,
    ) -> Result<DeviceDirectory, DirectoryError> {
        let url = format!("{}/{}", self.base_url, user_id.hyphenated());
        let response = self
            .session
            .execute(|c| c.get(&url).header(reqwest::header::ACCEPT, CBOR))
            .await?;

        let status = response.status();
        match status {
            reqwest::StatusCode::OK => {}
            reqwest::StatusCode::NOT_FOUND => return Err(DirectoryError::NotPublished),
            other => {
                return Err(DirectoryError::Unexpected {
                    status: other.as_u16(),
                });
            }
        }
        let bytes = response.bytes().await.map_err(DirectoryError::Body)?;
        let directory: DeviceDirectory = capsule_core::cbor::from_slice(&bytes)
            .map_err(|e| DirectoryError::Codec(e.to_string()))?;

        // Fail closed: an unverified directory is worse than none, because it would let a
        // server introduce a device key of its choosing into the trusted set.
        if !directory.verify(pinned_user_ik) {
            tracing::warn!("fetched device directory failed its user-IK signature check");
            return Err(DirectoryError::UntrustedSignature);
        }
        tracing::info!(
            directory_version = directory.core.directory_version,
            devices = directory.core.devices.len(),
            "device directory fetched and verified"
        );
        Ok(directory)
    }
}

/// Map a publish refusal onto its typed variant. Kept separate so the status table is one
/// readable list rather than a match arm buried in the request path.
fn publish_status_error(status: u16) -> DirectoryError {
    match status {
        400 => DirectoryError::Malformed,
        409 => DirectoryError::VersionConflict,
        other => DirectoryError::Unexpected { status: other },
    }
}

#[cfg(test)]
mod tests;
