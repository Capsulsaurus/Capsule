//! Web-upload guest drops — the drop store, staging inbox, and adoption transition
//! (contract skeleton; slice `S-C5` in the repo-root `SLICES.md`; SSoT:
//! <https://docs/design/web-upload/>).
//!
//! Guest-facing (link-capability auth, no account): `POST /u/{opaque-id}/drop` opens a
//! drop session (per-link caps + owner quota checked here; invariants 26–31), and chunks
//! reuse the upload protocol's `PATCH` mechanics. Owner-facing (session auth):
//! `GET /drops` lists the inbox, `POST /drops/{id}/adopt` runs the atomic inbox→album
//! promotion against the adopter's signed `create` manifest (invariant 32), and
//! `DELETE /drops/{id}` discards. A not-found, revoked, or expired link returns an
//! indistinguishable `404` — never `410`.

use salvo::oapi::extract::{JsonBody, PathParam};
use salvo::prelude::*;
use serde::{Deserialize, Serialize};

/// The unsigned guest descriptor uploaded beside the sealed ciphertext (the canonical
/// shape is `capsule_core::drop::DropDescriptor`; carried here as its JSON projection).
#[derive(Debug, Deserialize, ToSchema)]
#[allow(dead_code)]
pub(super) struct DropDescriptorBody {
    /// Closed enum for the link's pinned protocol version.
    pub content_type: String,
    /// Total plaintext byte length.
    pub plaintext_size: u64,
    /// STREAM plaintext chunk size.
    pub chunk_size: u32,
    /// STREAM nonce prefix (7 bytes, hex).
    pub nonce_prefix: String,
    /// Content address (hex) of the STREAM ciphertext.
    pub ciphertext_hash: String,
    /// `K` encapsulated to the link's Drop Key (base64); length fixed by the suite.
    pub kem_ct: String,
    /// Guest-supplied, unverified; advisory only.
    pub suggested_filename: Option<String>,
}

/// A drop session, ready to receive chunks.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct DropSessionResponse {
    /// The drop session id (chunks `PATCH` against it).
    pub drop_id: String,
}

/// One pending drop in the provisioning user's inbox.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct PendingDropResponse {
    /// The inbox row id.
    pub drop_id: String,
    /// The guest's descriptor.
    pub content_type: String,
    /// Declared plaintext size.
    pub plaintext_size: u64,
    /// Guest-supplied name, unverified.
    pub suggested_filename: Option<String>,
    /// Server-attested arrival time (RFC 3339).
    pub received_at: String,
}

/// The inbox listing.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct InboxResponse {
    /// Pending drops awaiting review.
    pub drops: Vec<PendingDropResponse>,
}

/// The adoption request: the adopter's signed `create` manifest (canonical CBOR,
/// base64) whose `ciphertext_hash` references the inbox blob, plus the freshly sealed
/// metadata blob it commits to.
#[derive(Debug, Deserialize, ToSchema)]
#[allow(dead_code)]
pub(super) struct AdoptRequest {
    /// The signed `create` manifest (canonical CBOR, base64), `key_mode = wrapped`.
    pub manifest_cbor: String,
    /// The encrypted metadata blob (base64) matching the manifest's `metadata_blob_hash`.
    pub metadata_blob: String,
}

/// The adoption result.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct AdoptResponse {
    /// The promoted asset's id.
    pub asset_id: String,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

/// Responses for guest drop-session creation.
#[allow(dead_code)]
pub(super) enum DropSessionResponses {
    /// Session opened.
    Created(DropSessionResponse),
    /// Link not found, revoked, or expired — indistinguishable by design (never `410`).
    NotFound,
    /// A per-link cap or the owner's quota refused the session.
    Refused(String),
}

#[async_trait]
impl Writer for DropSessionResponses {
    async fn write(mut self, req: &mut Request, depot: &mut Depot, res: &mut Response) {
        match self {
            Self::Created(data) => {
                res.status_code(StatusCode::CREATED);
                Json(data).write(req, depot, res).await;
            }
            Self::NotFound => {
                res.status_code(StatusCode::NOT_FOUND);
            }
            Self::Refused(msg) => {
                res.status_code(StatusCode::FORBIDDEN);
                res.render(Json(ErrorResponse { error: msg }));
            }
        }
    }
}

impl EndpointOutRegister for DropSessionResponses {
    fn register(components: &mut salvo::oapi::Components, operation: &mut salvo::oapi::Operation) {
        operation.responses.insert(
            String::from("201"),
            salvo::oapi::Response::new("Drop session opened").add_content(
                "application/json",
                salvo::oapi::Content::new(DropSessionResponse::to_schema(components)),
            ),
        );
        operation.responses.insert(
            String::from("404"),
            salvo::oapi::Response::new("Link not found, revoked, or expired (indistinguishable)"),
        );
        operation.responses.insert(
            String::from("403"),
            salvo::oapi::Response::new("Cap or quota refused the session"),
        );
    }
}

/// Responses for owner-facing inbox operations.
#[allow(dead_code)]
pub(super) enum InboxResponses {
    /// Inbox listed.
    Ok(InboxResponse),
}

#[async_trait]
impl Writer for InboxResponses {
    async fn write(mut self, req: &mut Request, depot: &mut Depot, res: &mut Response) {
        match self {
            Self::Ok(data) => {
                res.status_code(StatusCode::OK);
                Json(data).write(req, depot, res).await;
            }
        }
    }
}

impl EndpointOutRegister for InboxResponses {
    fn register(components: &mut salvo::oapi::Components, operation: &mut salvo::oapi::Operation) {
        operation.responses.insert(
            String::from("200"),
            salvo::oapi::Response::new("Pending drops").add_content(
                "application/json",
                salvo::oapi::Content::new(InboxResponse::to_schema(components)),
            ),
        );
    }
}

/// Responses for adoption / discard.
#[allow(dead_code)]
pub(super) enum AdoptResponses {
    /// The blob was atomically promoted from inbox to album asset.
    Ok(AdoptResponse),
    /// The drop is not in the caller's inbox.
    NotFound,
    /// The manifest failed the envelope invariants (1–8, 16–18, 25, 32).
    Rejected(String),
}

#[async_trait]
impl Writer for AdoptResponses {
    async fn write(mut self, req: &mut Request, depot: &mut Depot, res: &mut Response) {
        match self {
            Self::Ok(data) => {
                res.status_code(StatusCode::OK);
                Json(data).write(req, depot, res).await;
            }
            Self::NotFound => {
                res.status_code(StatusCode::NOT_FOUND);
            }
            Self::Rejected(msg) => {
                res.status_code(StatusCode::BAD_REQUEST);
                res.render(Json(ErrorResponse { error: msg }));
            }
        }
    }
}

impl EndpointOutRegister for AdoptResponses {
    fn register(components: &mut salvo::oapi::Components, operation: &mut salvo::oapi::Operation) {
        operation.responses.insert(
            String::from("200"),
            salvo::oapi::Response::new("Drop promoted to album asset").add_content(
                "application/json",
                salvo::oapi::Content::new(AdoptResponse::to_schema(components)),
            ),
        );
        operation.responses.insert(
            String::from("404"),
            salvo::oapi::Response::new("Drop not found in the caller's inbox"),
        );
        operation.responses.insert(
            String::from("400"),
            salvo::oapi::Response::new("Envelope invariants rejected the manifest"),
        );
    }
}

/// Open a drop session against a live upload link (guest; link-capability auth).
#[endpoint(operation_id = "create_drop_session", tags("drops"))]
pub async fn create_drop_session(
    _req: &mut Request,
    _depot: &mut Depot,
    _opaque_id: PathParam<String>,
    _body: JsonBody<DropDescriptorBody>,
) -> DropSessionResponses {
    todo!("S-C5: drop-session creation (invariants 26-31) — see SLICES.md")
}

/// List the provisioning user's pending drops (owner; session auth).
#[endpoint(operation_id = "list_drop_inbox", tags("drops"), security(("bearer" = [])))]
pub async fn list_drop_inbox(_req: &mut Request, _depot: &mut Depot) -> InboxResponses {
    todo!("S-C5: drop inbox listing — see SLICES.md")
}

/// Adopt a pending drop: validate the adopter's `create` manifest and atomically promote
/// the inbox blob to an album asset (owner; session auth).
#[endpoint(operation_id = "adopt_drop", tags("drops"), security(("bearer" = [])))]
pub async fn adopt_drop(
    _req: &mut Request,
    _depot: &mut Depot,
    _drop_id: PathParam<String>,
    _body: JsonBody<AdoptRequest>,
) -> AdoptResponses {
    todo!("S-C5: atomic inbox-to-album adoption (invariant 32) — see SLICES.md")
}

/// Discard a pending drop; its bytes are GC'd and the owner's quota freed (owner;
/// session auth).
#[endpoint(operation_id = "discard_drop", tags("drops"), security(("bearer" = [])))]
pub async fn discard_drop(
    _req: &mut Request,
    _depot: &mut Depot,
    _drop_id: PathParam<String>,
) -> AdoptResponses {
    todo!("S-C5: drop discard — see SLICES.md")
}
