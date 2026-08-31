//! Device-directory publish and fetch (slice `S-C9`).
//!
//! A user publishes their master-signed [`DeviceDirectory`] as opaque canonical CBOR; any
//! authenticated caller fetches a user's directory to pin it and to verify the manifests that
//! directory's devices signed. Nothing downstream works without this: a sync consumer that
//! cannot resolve a device key cannot verify anything it receives.
//!
//! [`crate::directory`] owns the port, the invariant-23 guard and the reason the guard lives in
//! the store rather than here. This module is the wire shape and its refusals.
//!
//! # Publishing is scoped by what was *signed*, not by the request
//!
//! A caller may publish only their own directory, and "their own" is decided by the `user_id`
//! inside the signed core rather than by a path parameter or the token alone. A document signed
//! for one account cannot be published under another's name even by the account that holds it.
//!
//! # Fetching is not scoped, and that is the design
//!
//! Any authenticated caller may fetch any user's directory, because that is what a directory is
//! *for*: Alice's device fetches Bob's to learn which keys to trust before adding him to an
//! album. The document is public by construction — it carries device public keys and a
//! signature, and nothing else.
//!
//! # `S-C28` audit
//!
//! | Salvo status | Verdict |
//! | --- | --- |
//! | publish `200` | kept |
//! | publish `400 error.directory.malformed` | kept, and now also covers a document signed for another account and one past the size ceiling |
//! | publish `415 error.directory.unsupported_media_type` | **new, and it replaces a phantom.** Kynos's own `Binary` rejection declares `400`, `415` *and* a `422` a raw-bytes body cannot produce, and carries no `error.*` code; [`DirectoryBody`] delegates the enforcement and replaces only the rejection — see [`crate::body`] |
//! | publish `409 error.directory.version_conflict` | kept — invariant 23, and the one status the whole surface exists for |
//! | fetch `200` (`application/cbor`, verbatim) | kept |
//! | fetch `404` | kept, and now carries `error.directory.not_published`; the Salvo body had no code |
//! | `401` | kept, and now the framework's |
//! | `500` | kept, with `error.directory.unavailable` |
//!
//! The retired `GET .../directory/{user_id}` was one of the four Salvo operations spargen 0.4
//! refuses outright: it carried a path template variable and declared **no path parameters**, so
//! no typed client could call it. Kynos checks at compile time that a path type's fields are
//! exactly the template's variables, which is why that defect is not expressible here.
//!
//! [`DeviceDirectory`]: capsule_core::crypto::keys::DeviceDirectory

use capsule_i18n::error_codes;
use kynos::extract::body::binary::Binary;
use kynos::extract::media::MediaType;
use kynos::prelude::*;
use serde::{Deserialize, Serialize};

use crate::auth::AccessToken;
use crate::body::{CodedMedia, OpaqueBody};
use crate::directory::{
    DeviceDirectoryContext, MalformedDirectory, PublishOutcome, PublishedDirectory,
};
use crate::store::UserId;

/// The device surface: who a user's devices are, and which keys to trust for them.
#[derive(Tag)]
#[tag(
    name = "devices",
    description = "Publishing and fetching the signed device directory."
)]
pub struct DevicesTag;

/// `application/cbor` — the encoding the directory is *signed* in.
///
/// A vendor marker rather than a reach for `application/octet-stream`: these bytes are not
/// opaque like a ciphertext blob, they are a canonical-CBOR document with a schema the client
/// knows. Saying so in the media type is what lets a generated client decode without guessing.
#[derive(Clone, Copy, Debug)]
pub struct Cbor;

impl MediaType for Cbor {
    const MEDIA_TYPE: &'static str = "application/cbor";
}

/// A body that is not CBOR, or that did not arrive whole, is refused with a directory code —
/// see [`crate::body`] for why the framework's own body rejection is not used directly.
impl CodedMedia for Cbor {
    const UNSUPPORTED_MEDIA_TYPE: &'static str = error_codes::DIRECTORY_UNSUPPORTED_MEDIA_TYPE;
    const UNREADABLE: &'static str = error_codes::DIRECTORY_MALFORMED;
}

/// The signed device directory, as it arrives.
pub type DirectoryBody = OpaqueBody<Cbor>;

/// The account whose directory is being fetched.
#[derive(PathParams, Schema)]
pub struct DirectoryPath {
    /// The account id.
    pub user_id: String,
}

/// The accepted version, echoed so a client knows what is now in force.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PublishDirectoryResponse {
    /// The version now stored, which equals the submitted one.
    pub directory_version: u64,
}

/// Why a directory was not published.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum PublishRejection {
    /// The body is not a signed directory this account may publish.
    #[error("{detail}")]
    #[problem(status = 400, title = "Malformed device directory")]
    Malformed {
        /// What was wrong, in English.
        detail: String,
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// Invariant 23: the version does not strictly advance.
    ///
    /// Carries both numbers. That is not a disclosure — the caller may fetch their own
    /// directory and read the stored version anyway — and it is the difference between a client
    /// that re-reads and republishes and one that retries the same losing document forever.
    #[error("directory version {submitted} does not advance the stored version {stored}")]
    #[problem(status = 409, title = "Directory version conflict")]
    VersionConflict {
        /// The version currently in force.
        #[problem(extension)]
        stored: u64,
        /// The version that was offered.
        #[problem(extension)]
        submitted: u64,
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The store could not answer.
    #[error("the device directory could not be published")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// Why a directory was not returned.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum FetchRejection {
    /// This account has never published a directory.
    #[error("no device directory has been published for this account")]
    #[problem(status = 404, title = "Not found")]
    NotPublished {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The store could not answer.
    #[error("the device directory could not be read")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// Publish the caller's signed device directory.
///
/// The bytes are stored verbatim; the server decodes them to read `directory_version` and
/// nothing else. The monotonicity comparison is the store's, not this handler's — see
/// [`crate::directory`] for why a read-compare-write here would be a rollback window.
#[kynos::post(
    "/v1/auth/devices/directory",
    operation_id = "publish_device_directory",
    tag = DevicesTag
)]
pub async fn publish_device_directory(
    Inject(directories): Inject<DeviceDirectoryContext>,
    Auth(credential): Auth<AccessToken>,
    body: DirectoryBody,
) -> Result<Json<PublishDirectoryResponse>, PublishRejection> {
    let user = UserId::new(credential.user.as_str());
    let document = body.into_vec();

    let directory_version =
        crate::directory::project_version(&document, &user).map_err(|error| {
            tracing::info!(%user, %error, "a device directory publish was refused as malformed");
            PublishRejection::malformed(&error)
        })?;

    let outcome = directories
        .store()
        .publish(PublishedDirectory {
            user_id: user.clone(),
            directory_version,
            document,
            published_at: directories.clock().now(),
        })
        .await
        .map_err(|error| {
            tracing::error!(%error, %user, "the device directory store could not publish");
            PublishRejection::unavailable()
        })?;

    match outcome {
        PublishOutcome::Published { directory_version } => {
            Ok(Json(PublishDirectoryResponse { directory_version }))
        }
        PublishOutcome::Stale { stored } => Err(PublishRejection::VersionConflict {
            stored,
            submitted: directory_version,
            code: error_codes::DIRECTORY_VERSION_CONFLICT,
        }),
    }
}

/// Fetch a user's signed device directory, verbatim.
///
/// The response body is the exact bytes the owner signed. Re-encoding them would detach the
/// document from its signature, and the failure would look like the *publisher's* bug.
#[kynos::get(
    "/v1/auth/devices/directory/{user_id}",
    operation_id = "fetch_device_directory",
    tag = DevicesTag
)]
pub async fn fetch_device_directory(
    Inject(directories): Inject<DeviceDirectoryContext>,
    Auth(credential): Auth<AccessToken>,
    Path(path): Path<DirectoryPath>,
) -> Result<Binary<Cbor>, FetchRejection> {
    let target = UserId::new(&path.user_id);
    let published = directories
        .store()
        .fetch(&target)
        .await
        .map_err(|error| {
            tracing::error!(%error, user = %target, "the device directory store could not answer");
            FetchRejection::Unavailable {
                code: error_codes::DIRECTORY_UNAVAILABLE,
            }
        })?
        .ok_or_else(|| {
            tracing::debug!(user = %target, "no device directory has been published");
            FetchRejection::NotPublished {
                code: error_codes::DIRECTORY_NOT_PUBLISHED,
            }
        })?;

    tracing::debug!(
        caller = %credential.user,
        user = %target,
        version = published.directory_version,
        "serving a device directory"
    );
    Ok(Binary::new(published.document))
}

impl PublishRejection {
    /// The body was not a directory this account may publish.
    fn malformed(error: &MalformedDirectory) -> Self {
        Self::Malformed {
            detail: error.to_string(),
            code: error_codes::DIRECTORY_MALFORMED,
        }
    }

    /// The store could not answer.
    fn unavailable() -> Self {
        Self::Unavailable {
            code: error_codes::DIRECTORY_UNAVAILABLE,
        }
    }
}
