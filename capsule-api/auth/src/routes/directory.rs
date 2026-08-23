//! Device-directory publish/fetch — the signed-directory server surface (slice `S-C9`;
//! SSoT: [Cryptography — Device Directory] and [Validation invariant 23]).
//!
//! A signed-in user **publishes** their master-signed [`DeviceDirectory`] as opaque
//! canonical CBOR; any authenticated caller **fetches** a user's directory to pin it and
//! verify manifests. The server stores and serves the signed bytes verbatim — it never
//! re-models the document. Its one semantic check is the anti-rollback guard (invariant 23):
//! a publish whose `directory_version` does not strictly advance the stored version is
//! refused `409 error.directory.version_conflict`, so the server cannot walk a directory back
//! to un-revoke a device.
//!
//! Distinct from `devices/enroll` (the S-C7 enrollment ceremony) and from `GET /auth/devices`
//! (the session listing). Paths live under `devices/directory`.
//!
//! [`DeviceDirectory`]: capsule_core::crypto::keys::DeviceDirectory
//! [Cryptography — Device Directory]: https://docs/design/cryptography/keys/#device-directory
//! [Validation invariant 23]: https://docs/design/threat-model/validation/

use capsule_i18n::error_codes;
use salvo::prelude::*;
use serde::Serialize;
use service::directory::{DirectoryError, Mutation, Query};

use crate::errors::ClaimValidationError;
use crate::models::errors::ApiError;
use crate::state::AppState;
use crate::utils::headers::get_token_from_headers;

/// Upper bound on a published directory body. A hybrid-signed directory of many devices is a
/// few hundred KiB at most; a larger body is refused before buffering.
const MAX_DIRECTORY_BYTES: usize = 512 * 1024;

/// The accepted-version acknowledgement for a successful publish.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct PublishDirectoryResponse {
    /// The `directory_version` now stored for the user (equals the submitted version).
    pub directory_version: i64,
}

/// Resolve the caller's account id from the bearer access token, or the auth error to
/// surface. Shared by publish and fetch.
fn authenticate(req: &Request, state: &AppState) -> Result<String, ClaimValidationError> {
    let token = get_token_from_headers(req.headers())?;
    use secrecy::ExposeSecret;
    let claims = state.auth_service.get_claims(token.expose_secret())?;
    claims.validate_access_token()?;
    Ok(claims.sub)
}

/// Publish responses. Every rejection carries a stable `error.*` code (clients switch on the
/// code, not the bare status).
pub(super) enum PublishDirectoryResponses {
    /// Stored; the accepted version is echoed back.
    Ok(PublishDirectoryResponse),
    /// No/invalid bearer token.
    Unauthorized(ClaimValidationError),
    /// Body is not a decodable signed directory (`400 error.directory.malformed`).
    Malformed(String),
    /// Invariant 23: the version does not strictly advance (`409 error.directory.version_conflict`).
    VersionConflict { stored: i64, submitted: i64 },
    /// Server fault.
    Internal,
}

capsule_wire::salvo_responses! {
    PublishDirectoryResponses {
        Ok(body) => 200, json(body),
            doc("Directory published", schema = PublishDirectoryResponse);
        Unauthorized(e) => _, delegate(e), undocumented();
        Malformed(detail) => 400, json(ApiError::with_code(
            format!("Malformed device directory: {detail}"),
            error_codes::DIRECTORY_MALFORMED,
        )), doc("Malformed device directory document");
        VersionConflict { stored, submitted } => 409, json(ApiError::with_code(
            format!(
                "Directory version {submitted} does not advance the stored version {stored}"
            ),
            error_codes::DIRECTORY_VERSION_CONFLICT,
        )), doc("Directory version does not advance (invariant 23)");
        Internal {} => 500, json(ApiError::new("Internal server error")), undocumented();
    }
    delegated {
        401 => "Missing or invalid access token",
    }
}

/// Fetch responses. The success body is the exact signed CBOR bytes last published.
pub(super) enum FetchDirectoryResponses {
    /// The verbatim signed directory bytes (`application/cbor`).
    Ok(Vec<u8>),
    /// No/invalid bearer token.
    Unauthorized(ClaimValidationError),
    /// The user has never published a directory.
    NotFound,
    /// Server fault.
    Internal,
}

capsule_wire::salvo_responses! {
    FetchDirectoryResponses {
        Ok(bytes) => 200,
            header("Content-Type", "application/cbor")
            custom { |res|
                if let Err(e) = res.write_body(bytes) {
                    tracing::error!("failed to write directory body: {e}");
                    res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
                }
            },
            doc(
                "The signed device directory, returned verbatim as opaque CBOR (application/cbor)"
            );
        Unauthorized(e) => _, delegate(e), undocumented();
        NotFound {} => 404, json(ApiError::new(
            "No device directory published for this user",
        )), doc("No directory published for this user");
        Internal {} => 500, json(ApiError::new("Internal server error")), undocumented();
    }
    delegated {
        401 => "Missing or invalid access token",
    }
}

/// Publish the caller's signed device directory. The body is the opaque signed CBOR; the
/// server projects only `directory_version` for the invariant-23 monotonicity check and
/// stores the bytes verbatim.
#[endpoint(operation_id = "publish_device_directory", tags("auth"), security(("bearer" = [])))]
pub async fn publish_device_directory(
    req: &mut Request,
    depot: &mut Depot,
) -> PublishDirectoryResponses {
    let state = depot
        .obtain::<AppState>()
        .expect("AppState is injected by middleware");

    let user_id = match authenticate(req, state) {
        Ok(id) => id,
        Err(e) => return PublishDirectoryResponses::Unauthorized(e),
    };

    let body = match req.payload_with_max_size(MAX_DIRECTORY_BYTES).await {
        Ok(b) => b.to_vec(),
        Err(e) => {
            return PublishDirectoryResponses::Malformed(format!("unreadable body: {e}"));
        }
    };

    match Mutation::publish(&state.conn, &user_id, body).await {
        Ok(directory_version) => {
            PublishDirectoryResponses::Ok(PublishDirectoryResponse { directory_version })
        }
        Err(DirectoryError::Malformed(detail)) => PublishDirectoryResponses::Malformed(detail),
        Err(DirectoryError::VersionConflict { stored, submitted }) => {
            PublishDirectoryResponses::VersionConflict { stored, submitted }
        }
        Err(DirectoryError::Db(e)) => {
            tracing::error!("device directory publish db error: {e}");
            PublishDirectoryResponses::Internal
        }
    }
}

/// Fetch a user's signed device directory verbatim so a client can pin it and verify the
/// signature it covers.
#[endpoint(operation_id = "fetch_device_directory", tags("auth"), security(("bearer" = [])))]
pub async fn fetch_device_directory(
    req: &mut Request,
    depot: &mut Depot,
) -> FetchDirectoryResponses {
    let state = depot
        .obtain::<AppState>()
        .expect("AppState is injected by middleware");

    if let Err(e) = authenticate(req, state) {
        return FetchDirectoryResponses::Unauthorized(e);
    }

    let target_user_id = req.param::<String>("user_id").unwrap_or_default();

    match Query::fetch(&state.conn, &target_user_id).await {
        Ok(Some(bytes)) => FetchDirectoryResponses::Ok(bytes),
        Ok(None) => FetchDirectoryResponses::NotFound,
        Err(e) => {
            tracing::error!("device directory fetch db error: {e}");
            FetchDirectoryResponses::Internal
        }
    }
}
