//! `GET /v1/blob/{hash}` — key-free ranged blob serving (slice `S-C10`).
//!
//! The read side of the store. A client learns an address from a sync entry and fetches the
//! ciphertext here, resumably: [Encryption — ranged reads] fixes the client's stride at the
//! 65,536-byte ciphertext chunk so every fetched span decrypts in isolation, and this surface
//! honours whatever `Range` arrives without knowing why it was chosen. The decision the server
//! makes is [`crate::serve::resolve`]'s; this module is the wire shape around it.
//!
//! # What Kynos does here that the Salvo surface did by hand
//!
//! The retired route built a `NamedFile` and let it write the range, which tied resumable
//! serving to the blob store being a **filesystem**. Kynos's `ByteSource` is a trait over
//! spans, so the range rides on the blob *port* instead: an object-store adapter resumes with
//! nothing above it changing, and the tests range over a `BTreeMap`. `Conditions` arrives as
//! one extractor because RFC 9110 fixes the order `Range`, `If-Range`, `If-None-Match` and
//! `If-Modified-Since` are evaluated in — taking them separately is how they get applied in the
//! wrong one — and taking it is also what puts all four in the emitted document.
//!
//! The content address **is** a strong validator, so `ETag` is not a construction here but the
//! name itself. That is what makes `If-Range` honest: a resumed fetch cannot splice bytes from
//! a different representation, because a different representation has a different address.
//!
//! # `S-C28` audit
//!
//! | Salvo status | Verdict |
//! | --- | --- |
//! | `200`/`206`/`416` | kept, and now derived — `Delivery` declares all three from its own type |
//! | `404` | kept. Bodyless there, an RFC 9457 problem here: every `404` this route renders is byte-identical, so it discriminates nothing and gains the `error.*` code the i18n contract requires |
//! | `410` | kept |
//! | `409 error.blob.pending_upload` | **restored by `S-C40`**, and rendered from the caller's own in-flight upload rather than from a reference the index does not have — see [`crate::serve`] |
//! | `401` | kept, and now the framework's, with the `WWW-Authenticate` challenge |
//! | `500` | kept, with `error.blob.unavailable` |
//!
//! [Encryption — ranged reads]: ../../../capsule-docs/src/content/docs/design/cryptography/encryption.md

use capsule_i18n::error_codes;
use kynos::extract::media::OctetStream;
use kynos::http::etag::ETag;
use kynos::prelude::*;
use kynos::response::range::served::{Conditions, Delivery, Served};

use crate::auth::AccessToken;
use crate::serve::{BlobSource, ServeContext, ServeResolution};

/// The media surface: fetching the opaque ciphertext a sync entry named.
#[derive(Tag)]
#[tag(
    name = "media",
    description = "Fetching content-addressed ciphertext, resumably."
)]
pub struct MediaTag;

/// The content address in the path.
#[derive(PathParams, Schema)]
pub struct BlobPath {
    /// The blob's ciphertext content address, lowercase hex.
    pub hash: String,
}

/// Why no bytes were served.
///
/// The split between these four is the whole contract, so each says what a client should do:
/// `404` and `410` are permanent and the client degrades to a representation it already holds;
/// `409` is **transient** and the client waits; `500` is the server's and is retried. Collapsing
/// `409` into either neighbour is what makes a client give up on bytes that are on their way, or
/// retry forever for bytes that are not.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum BlobRejection {
    /// No live reference names the address — or it is not an address.
    ///
    /// One body for both, deliberately: a malformed address and an unknown one must be
    /// indistinguishable, or this route becomes an oracle over guesses.
    #[error("no such blob")]
    #[problem(status = 404, title = "Not found")]
    NotFound {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// Not referenced *yet*: the caller's own account has an upload of exactly these bytes in
    /// flight (`S-C40`). Transient.
    ///
    /// Carries no session identifier and no progress. The fetcher is a different device from
    /// the uploader, and telling it *which* upload or how far along would be reporting on
    /// another device's transfer to satisfy nothing a client can act on — the action is "wait
    /// and retry", which the status alone says.
    #[error("the original has not finished uploading yet")]
    #[problem(status = 409, title = "Upload in progress")]
    Pending {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// Referenced, but not retrievable per policy. Permanent.
    #[error("this blob is no longer available")]
    #[problem(status = 410, title = "Gone")]
    Gone {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A collaborator could not answer, so nothing was decided.
    #[error("the blob could not be served")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

impl BlobRejection {
    /// No live reference, or a malformed address.
    fn not_found() -> Self {
        Self::NotFound {
            code: error_codes::BLOB_NOT_FOUND,
        }
    }

    /// On its way.
    fn pending() -> Self {
        Self::Pending {
            code: error_codes::BLOB_PENDING_UPLOAD,
        }
    }

    /// Referenced but gone.
    fn gone() -> Self {
        Self::Gone {
            code: error_codes::BLOB_GONE,
        }
    }

    /// A collaborator could not answer.
    fn unavailable() -> Self {
        Self::Unavailable {
            code: error_codes::BLOB_UNAVAILABLE,
        }
    }
}

/// Fetch a ciphertext blob by its content address, ranged.
///
/// Opaque octets: the server holds no key and this route never learns what it is serving. Any
/// authenticated account may fetch any live address — see [`crate::serve`] for why that is a
/// capability model rather than a hole, and for the `403` the contract describes and nothing
/// implements.
///
/// The one answer that *is* account-scoped is the transient `409`: it reports the caller's own
/// in-flight upload and nobody else's (`S-C40`).
#[kynos::get("/v1/blob/{hash}", operation_id = "get_blob", tag = MediaTag)]
pub async fn get_blob(
    Inject(serve): Inject<ServeContext>,
    Auth(credential): Auth<AccessToken>,
    Path(path): Path<BlobPath>,
    conditions: Conditions,
) -> Result<Delivery<OctetStream>, BlobRejection> {
    // The caller files under itself. Nothing about *reading* is scoped by it today — any
    // authenticated account may fetch any live address, see [`crate::serve`] — but the
    // transient `409` is, and `S-C39` is where the read authority that would scope the rest
    // arrives.
    let owner = crate::store::OwnerId::new(credential.user.as_str());
    let resolution = crate::serve::resolve(&serve, &owner, &path.hash)
        .await
        .map_err(|error| {
            tracing::error!(%error, user = %credential.user, "a blob fetch could not be resolved");
            BlobRejection::unavailable()
        })?;

    let (address, size) = match resolution {
        ServeResolution::Serve { address, size } => (address, size),
        ServeResolution::AwaitingUpload { .. } => return Err(BlobRejection::pending()),
        ServeResolution::NotFound => return Err(BlobRejection::not_found()),
        ServeResolution::Gone => return Err(BlobRejection::gone()),
    };

    // The address is the validator. Strong because it is a hash of the bytes themselves, which
    // is the only claim under which `If-Range` is defined — and one this store can make
    // honestly, unlike a mtime.
    let etag = ETag::strong(address.as_str());
    let source = BlobSource::new(serve.blob_handle(), address, size);

    Served::<_, OctetStream>::new(source)
        .etag(etag)
        // Immutable by construction: the bytes at an address cannot change without changing the
        // address, so a client may keep them for as long as it likes. `private` because a blob
        // is one account's ciphertext and a shared cache has no business holding it.
        .cache_control("private, max-age=31536000, immutable")
        .deliver(&conditions)
        .await
        .map_err(|error| {
            tracing::error!(%error, "a blob vanished between resolution and delivery");
            BlobRejection::unavailable()
        })
}
