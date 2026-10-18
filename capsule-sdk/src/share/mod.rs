//! The owner's two share-link operations over the generated client (#622): register a link
//! the issuing client published (`POST /v1/shares`), and revoke one (`DELETE
//! /v1/shares/{opaque_id}`).
//!
//! Orchestration only. The link and everything it serves are built in `capsule-core`:
//! [`ShareLinkIssuer::create_link`](capsule_core::sharing::ShareLinkIssuer::create_link) mints
//! the opaque id, the fragment secret and the scope encapsulation, and
//! `Workspace::publish_link` seals the `share-metadata/v1` record into a
//! [`SharePublication`]. This module maps the two onto the generated `issue_share` request,
//! calls the generated operations through [`AuthenticatedClient`] (so the bearer, the
//! pre-flight refresh and the one `401` replay are the SDK's), and maps the generated typed
//! refusals onto [`ShareError`]. Nothing here parses or serializes by hand.
//!
//! What this module does **not** do is put the metadata blob on the server. `serves` names it
//! and the public path serves it from the blob store, but no write surface accepts it yet, so a
//! link registered here opens only once that blob is in the store.
//!
//! The fragment secret never reaches this module's requests: [`issue_request`] reads the opaque
//! id, the publication and the expiry, and the URL is assembled locally by [`share_url`].

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_core::sharing::{ShareLink, SharePublication};
use tracing::instrument;

use crate::auth::Session;
use crate::client::{AuthenticatedClient, ClientError, describe_error, is_token_failure};
use crate::rest;

/// Why a share-link operation did not complete.
#[derive(Debug, thiserror::Error)]
pub enum ShareError {
    /// The configured API root is not a URL operation paths can hang off.
    #[error("invalid base URL {url:?}: {reason}")]
    InvalidBaseUrl {
        /// The offending URL.
        url: String,
        /// Why it was refused.
        reason: String,
    },
    /// The server refused the link record (`400 error.share.malformed`, `415`, `422`): sending
    /// the same request again will not help.
    #[error("the share link was refused: {detail}")]
    Malformed {
        /// The server's stable `error.*` code.
        code: Option<String>,
        /// The server's English detail.
        detail: String,
    },
    /// The credential was refused (`401`/`403`), or the session could not produce one.
    #[error("the share request was not authorized")]
    Unauthorized {
        /// The server's stable `error.*` code, when it answered.
        code: Option<String>,
    },
    /// The server could not answer (`500 error.share.unavailable`); retrying later may work.
    #[error("the share store is unavailable")]
    Unavailable {
        /// The server's stable `error.*` code.
        code: Option<String>,
    },
    /// The server answered with a status this client does not model, such as the protocol
    /// gate's `426` or the transport backstop's `413`.
    #[error("the share request was answered {status}")]
    Unexpected {
        /// The HTTP status.
        status: u16,
        /// The server's stable `error.*` code, when the body carried one.
        code: Option<String>,
    },
    /// The server answered `201` for a different link than the one registered.
    #[error("the server echoed link {echoed}, not {requested}")]
    WrongEcho {
        /// The opaque id this client registered.
        requested: String,
        /// The opaque id the server echoed.
        echoed: String,
    },
    /// The request did not complete.
    #[error("the share request did not complete: {0}")]
    Transport(String),
}

impl ShareError {
    /// The stable `error.*` catalog code a client localizes, when the server sent one.
    #[must_use]
    pub fn error_code(&self) -> Option<&str> {
        match self {
            Self::Malformed { code, .. }
            | Self::Unauthorized { code }
            | Self::Unavailable { code }
            | Self::Unexpected { code, .. } => code.as_deref(),
            Self::InvalidBaseUrl { .. } | Self::WrongEcho { .. } | Self::Transport(_) => None,
        }
    }
}

/// The `POST /v1/shares` body for `link`, from what `Workspace::publish_link` returned for it.
///
/// The record's `metadata_hash` and `serves` (record first, then each covered ciphertext) are
/// carried as lowercase hex, the passphrase-wrapped scope as base64, and the expiry as the
/// link's own RFC 3339 string. The fragment secret is not read.
#[must_use]
pub fn issue_request(
    link: &ShareLink,
    publication: &SharePublication,
) -> rest::types::IssueShareRequest {
    rest::types::IssueShareRequest {
        opaque_id: link.opaque_id_hex(),
        metadata_hash: publication.metadata_hash.to_hex(),
        serves: publication
            .serves
            .iter()
            .map(capsule_core::crypto::hash::Hash32::to_hex)
            .collect(),
        wrapped_secret: publication
            .wrapped_secret
            .as_ref()
            .map(|secret| BASE64.encode(secret)),
        expires_at: link.expires_at.clone(),
    }
}

/// The URL a recipient opens: `{origin}/s/{opaque_id}#{fragment}`.
///
/// `origin` is the server's public origin (`https://capsule.example.com`); a trailing `/` is
/// dropped so the path is not doubled. The fragment is what makes the link work and is never
/// sent to the server by a browser, which is why it rides after the `#`.
#[must_use]
pub fn share_url(origin: &str, link: &ShareLink) -> String {
    format!(
        "{}/s/{}#{}",
        origin.trim_end_matches('/'),
        link.opaque_id_hex(),
        link.fragment_hex()
    )
}

/// Registers and revokes the caller's share links.
pub struct ShareClient {
    client: AuthenticatedClient,
}

impl ShareClient {
    /// Build a share client against the **API root** (e.g. `https://api.example.com`), the
    /// origin the generated operation paths hang off.
    ///
    /// # Errors
    ///
    /// [`ShareError::InvalidBaseUrl`] when `api_base_url` does not parse.
    pub fn new(session: Session, api_base_url: &str) -> Result<Self, ShareError> {
        let client =
            AuthenticatedClient::new(api_base_url, session).map_err(|error| match error {
                ClientError::InvalidBaseUrl { url, reason } => {
                    ShareError::InvalidBaseUrl { url, reason }
                }
            })?;
        Ok(Self { client })
    }

    /// Register `link` with the server, so its public path serves what `publication` names.
    ///
    /// An opaque id registers once. The server refuses any id it already holds, the caller's
    /// own included, with the same `400 error.share.malformed` it gives a malformed record
    /// (#553), so re-sending a request whose first attempt landed surfaces as
    /// [`ShareError::Malformed`], not as success. That refusal is not read as success here: it
    /// is also the answer for an id the caller revoked, and its English detail is not a stable
    /// discriminator. A caller whose request timed out recovers by revoking the id (revoke
    /// answers `204` whether or not the first attempt landed) and issuing a freshly minted
    /// link.
    ///
    /// # Errors
    ///
    /// The refusals [`ShareError`] names, and [`ShareError::WrongEcho`] when the server
    /// confirms a different link.
    #[instrument(skip_all, fields(opaque_id = %link.opaque_id_hex(), serves = publication.serves.len()))]
    pub async fn issue(
        &self,
        link: &ShareLink,
        publication: &SharePublication,
    ) -> Result<(), ShareError> {
        let body = issue_request(link, publication);
        let echoed = self
            .client
            .issue_share(
                capsule_core::crypto::primitives::PROTOCOL_VERSION,
                None,
                &body,
            )
            .await
            .map_err(|error| refusal(error, issue_refusal))?
            .into_inner()
            .opaque_id;
        if echoed != body.opaque_id {
            return Err(ShareError::WrongEcho {
                requested: body.opaque_id,
                echoed,
            });
        }
        tracing::info!(
            passphrase = body.wrapped_secret.is_some(),
            expires = body.expires_at.is_some(),
            "share link registered"
        );
        Ok(())
    }

    /// Revoke the caller's link `opaque_id` (lowercase hex).
    ///
    /// The server answers `204` whether the link was the caller's, was already revoked, or never
    /// existed, so success says the link is not live for this caller, not that one was revoked.
    ///
    /// # Errors
    ///
    /// The refusals [`ShareError`] names.
    #[instrument(skip(self))]
    pub async fn revoke(&self, opaque_id: &str) -> Result<(), ShareError> {
        self.client
            .revoke_share(
                opaque_id.to_owned(),
                capsule_core::crypto::primitives::PROTOCOL_VERSION,
                None,
            )
            .await
            .map_err(|error| refusal(error, revoke_refusal))?;
        tracing::info!("share link revocation accepted");
        Ok(())
    }
}

/// Map a generated-client failure: a declared refusal through `declared`, everything else to a
/// wire class.
fn refusal<E>(error: rest::Error<E>, declared: fn(u16, E) -> ShareError) -> ShareError
where
    E: std::error::Error + 'static,
{
    match error {
        rest::Error::Api(response) => {
            let status = response.status().as_u16();
            let mapped = declared(status, response.into_inner());
            tracing::warn!(status, code = ?mapped.error_code(), "share request refused");
            mapped
        }
        rest::Error::UnexpectedStatus { status, .. } => ShareError::Unexpected {
            status: status.as_u16(),
            code: None,
        },
        rest::Error::RequestConstruction(_) if is_token_failure(&error) => {
            ShareError::Unauthorized { code: None }
        }
        other => ShareError::Transport(describe_error(&other)),
    }
}

fn coded(problem: &rest::types::CodedProblem) -> (Option<String>, String) {
    (
        Some(problem.code.clone()),
        problem.detail.clone().unwrap_or_default(),
    )
}

fn issue_refusal(status: u16, error: rest::IssueShareError) -> ShareError {
    use rest::IssueShareError as Refusal;
    match error {
        Refusal::Status400(problem) | Refusal::Status415(problem) | Refusal::Status422(problem) => {
            let (code, detail) = coded(&problem);
            ShareError::Malformed { code, detail }
        }
        Refusal::Status401(problem) | Refusal::Status403(problem) => ShareError::Unauthorized {
            code: Some(problem.code.clone()),
        },
        Refusal::Status500(problem) => ShareError::Unavailable {
            code: Some(problem.code.clone()),
        },
        Refusal::Status426(problem) => ShareError::Unexpected {
            status,
            code: Some(problem.code.clone()),
        },
        Refusal::Status413 => ShareError::Unexpected { status, code: None },
    }
}

fn revoke_refusal(status: u16, error: rest::RevokeShareError) -> ShareError {
    use rest::RevokeShareError as Refusal;
    match error {
        Refusal::Status400(problem) => {
            let (code, detail) = coded(&problem);
            ShareError::Malformed { code, detail }
        }
        Refusal::Status401(problem) | Refusal::Status403(problem) => ShareError::Unauthorized {
            code: Some(problem.code.clone()),
        },
        Refusal::Status500(problem) => ShareError::Unavailable {
            code: Some(problem.code.clone()),
        },
        Refusal::Status426(problem) => ShareError::Unexpected {
            status,
            code: Some(problem.code.clone()),
        },
        Refusal::Status413 => ShareError::Unexpected { status, code: None },
    }
}

#[cfg(test)]
mod tests;
