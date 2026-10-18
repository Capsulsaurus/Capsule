//! `capsule share create|revoke` — view-only share links issued against a real server (#654).
//!
//! The CLI is a client: every leg here is a primitive somebody else owns, and this module only
//! orders them.
//!
//! `create` runs, in this order:
//!
//! 1. **Scope check** — [`Workspace::shared_assets`] must cover at least one asset. A link over
//!    nothing opens to an empty page forever, because the record it serves is sealed at issue
//!    and never grows; refusing costs nothing, issuing it costs a dead URL.
//! 2. **Mint** — `ShareLinkIssuer::create_link` draws the opaque id and the fragment secret and
//!    encapsulates the scope (under an Argon2id layer for a `--protect` link).
//! 3. **Publish** — [`Workspace::publish_link`] seals the `share-metadata/v1` record and names
//!    every address the link may serve.
//! 4. **Custody check** — every covered ciphertext must already be held by the server, read off
//!    a fresh feed pull ([`remote::held_blobs`]). The server stores the metadata record from the
//!    issue request itself (#653), but it does not check the ciphertexts: a link registered over
//!    an original that was never pushed would answer `404` for that asset to every viewer, and
//!    the owner would have no way to tell why. So the CLI refuses and points at `capsule push`.
//! 5. **Register** — [`ShareClient::issue`], which carries the sealed record in the same request.
//! 6. **URL** — [`share_url`] against the server origin. The fragment never leaves this process
//!    except in the printed URL.
//!
//! `revoke` checks the opaque id's shape locally and calls [`ShareClient::revoke`]. The server
//! answers `204` whether the link was the caller's, was already revoked, or never existed, so a
//! success says the link is not live for this account, which is what the user asked for.
//!
//! The library's own link record ([`capsule_core::sharing::ShareLinkRecord`]) is in-memory and
//! is not persisted by the workspace, so revocation is server-side only, by opaque id.

use std::collections::HashSet;

use capsule_core::crypto::hash::Hash32;
use capsule_core::lifecycle::Workspace;
use capsule_core::sharing::{OPAQUE_ID_LEN, ShareLinkIssuer as _, ShareScope, SharingError};
use capsule_i18n::Bundle;
use capsule_sdk::share::{ShareClient, ShareError, share_url};
use colored::Colorize as _;
use thiserror::Error;
use tracing::instrument;

use crate::i18n::{Value, keys};
use crate::remote::{self, RemoteConfig, RemoteError};
use crate::session::SessionStore;

/// One `capsule share create` invocation, independent of the argument parser so the flow is
/// drivable (and testable) without a `clap` round trip.
///
/// `Debug` is hand-written so the link passphrase never reaches a log line.
#[derive(Clone)]
pub struct CreateRequest {
    /// What the link grants: one album or one asset.
    pub scope: ShareScope,
    /// The `--expires-at` value as typed; [`normalize_expiry`] checks it.
    pub expires_at: Option<String>,
    /// The link passphrase for a `--protect` link.
    pub passphrase: Option<String>,
}

impl std::fmt::Debug for CreateRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreateRequest")
            .field("scope", &self.scope)
            .field("expires_at", &self.expires_at)
            .field(
                "passphrase",
                &self.passphrase.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

/// What a successful `capsule share create` registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedShare {
    /// The link's opaque id, lowercase hex — what `capsule share revoke` takes.
    pub opaque_id: String,
    /// The URL a recipient opens, fragment included.
    pub url: String,
    /// How many assets the link covers.
    pub covered: usize,
    /// Whether the recipient also needs the link passphrase.
    pub passphrase_protected: bool,
    /// The normalized RFC 3339 expiry, if any.
    pub expires_at: Option<String>,
}

/// Everything `capsule share` can fail with.
#[derive(Debug, Error)]
pub enum ShareCommandError {
    /// `--expires-at` is not an RFC 3339 timestamp.
    #[error("expiry {0:?} is not an RFC 3339 timestamp")]
    InvalidExpiry(String),
    /// `--expires-at` is not after now, so the link would never open.
    #[error("expiry {0} is not in the future")]
    ExpiryNotInFuture(String),
    /// The revoke argument is not 32 lowercase hex characters.
    #[error("{0:?} is not a share link's opaque id")]
    InvalidOpaqueId(String),
    /// The scope covers no asset outside trash.
    #[error("the share scope covers no asset")]
    EmptyScope,
    /// Covered ciphertexts the server does not hold yet, as lowercase hex.
    #[error("{} covered ciphertext(s) are not on the server", missing.len())]
    NotHeld {
        /// The missing content addresses, in the order the record covers them.
        missing: Vec<String>,
    },
    /// The library could not mint or publish the link.
    #[error(transparent)]
    Sharing(#[from] SharingError),
    /// The server refused, or could not be reached for, the issue or the revoke.
    #[error(transparent)]
    Share(#[from] ShareError),
    /// The session or the custody check failed.
    #[error(transparent)]
    Remote(#[from] RemoteError),
}

impl ShareCommandError {
    /// The stable `error.*` catalog code a client localizes, when the server sent one.
    #[must_use]
    pub fn error_code(&self) -> Option<&str> {
        match self {
            Self::Share(error) => error.error_code(),
            Self::Remote(error) => error.error_code(),
            _ => None,
        }
    }
}

/// Check `raw` is an RFC 3339 timestamp after `now`, and return it in the canonical UTC form
/// the server and the sealed record both carry.
///
/// # Errors
///
/// [`ShareCommandError::InvalidExpiry`] when it does not parse, and
/// [`ShareCommandError::ExpiryNotInFuture`] when it is not after `now`.
pub fn normalize_expiry(raw: &str, now: jiff::Timestamp) -> Result<String, ShareCommandError> {
    let at: jiff::Timestamp = raw
        .trim()
        .parse()
        .map_err(|_| ShareCommandError::InvalidExpiry(raw.to_owned()))?;
    if at <= now {
        return Err(ShareCommandError::ExpiryNotInFuture(at.to_string()));
    }
    Ok(at.to_string())
}

/// Check `raw` is an opaque id this server could have issued: 32 lowercase hex characters, the
/// shape the server checks before any lookup. Surrounding whitespace is dropped.
///
/// # Errors
///
/// [`ShareCommandError::InvalidOpaqueId`] otherwise.
pub fn validate_opaque_id(raw: &str) -> Result<String, ShareCommandError> {
    let id = raw.trim();
    let well_formed = id.len() == OPAQUE_ID_LEN * 2
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if well_formed {
        Ok(id.to_owned())
    } else {
        Err(ShareCommandError::InvalidOpaqueId(raw.to_owned()))
    }
}

/// The covered ciphertexts in `serves` the server does not hold, as lowercase hex.
///
/// `serves` is [`capsule_core::sharing::SharePublication::serves`]: the metadata record first,
/// which travels in the issue request itself and is never on the feed, then each covered
/// ciphertext. So the first entry is skipped and the rest are checked against `held`.
#[must_use]
pub fn missing_ciphertexts<S: std::hash::BuildHasher>(
    serves: &[Hash32],
    held: &HashSet<String, S>,
) -> Vec<String> {
    serves
        .iter()
        .skip(1)
        .map(Hash32::to_hex)
        .filter(|hex| !held.contains(hex))
        .collect()
}

/// `capsule share create`: mint, publish, check custody, register, and return the URL.
///
/// `now` is what `--expires-at` is checked against; the binary passes the system clock.
///
/// # Errors
///
/// [`ShareCommandError`] — a local refusal before any request, a custody refusal, or the
/// server's refusal of the issue. A refusal leaves nothing registered.
#[instrument(skip(store, workspace, request), fields(scope = ?request.scope, protected = request.passphrase.is_some()))]
pub async fn create(
    remote: &RemoteConfig,
    store: &SessionStore,
    workspace: &mut Workspace,
    request: &CreateRequest,
    now: jiff::Timestamp,
) -> Result<IssuedShare, ShareCommandError> {
    let expires_at = request
        .expires_at
        .as_deref()
        .map(|raw| normalize_expiry(raw, now))
        .transpose()?;
    let covered = workspace.shared_assets(request.scope)?.len();
    if covered == 0 {
        tracing::info!("share: the scope covers no asset; nothing is issued");
        return Err(ShareCommandError::EmptyScope);
    }

    let session = remote::resume_session(remote, store).await?;
    let outcome = create_with(
        remote,
        session.clone(),
        workspace,
        request,
        expires_at,
        covered,
    )
    .await;
    remote::checkpoint(store, &session).await;
    outcome
}

async fn create_with(
    remote: &RemoteConfig,
    session: capsule_sdk::auth::Session,
    workspace: &mut Workspace,
    request: &CreateRequest,
    expires_at: Option<String>,
    covered: usize,
) -> Result<IssuedShare, ShareCommandError> {
    let link = workspace.create_link(
        request.scope,
        expires_at.clone(),
        request.passphrase.as_deref(),
    )?;
    let publication = workspace.publish_link(&link)?;

    let held = remote::held_blobs(remote, session.clone(), remote::DEFAULT_SYNC_PAGE_SIZE).await?;
    let missing = missing_ciphertexts(&publication.serves, &held);
    if !missing.is_empty() {
        tracing::warn!(
            missing = missing.len(),
            covered,
            "share: covered ciphertexts are not on the server; nothing is issued"
        );
        return Err(ShareCommandError::NotHeld { missing });
    }

    // `sync_endpoint` is the bare server origin: the API root the generated operations hang
    // off, and the origin that serves `/s/{opaque_id}`.
    let origin = &remote.sync_endpoint;
    ShareClient::new(session, origin)?
        .issue(&link, &publication)
        .await?;
    let issued = IssuedShare {
        opaque_id: link.opaque_id_hex(),
        url: share_url(origin, &link),
        covered,
        passphrase_protected: link.is_passphrase_protected(),
        expires_at,
    };
    tracing::info!(
        opaque_id = %issued.opaque_id,
        covered,
        passphrase = issued.passphrase_protected,
        expires = issued.expires_at.is_some(),
        "share: link issued"
    );
    Ok(issued)
}

/// `capsule share revoke`: stop the link `opaque_id` from opening.
///
/// # Errors
///
/// [`ShareCommandError::InvalidOpaqueId`] before any request, else what the session or the
/// server answered.
#[instrument(skip(store))]
pub async fn revoke(
    remote: &RemoteConfig,
    store: &SessionStore,
    opaque_id: &str,
) -> Result<String, ShareCommandError> {
    let id = validate_opaque_id(opaque_id)?;
    let session = remote::resume_session(remote, store).await?;
    let outcome = match ShareClient::new(session.clone(), &remote.sync_endpoint) {
        Ok(client) => client.revoke(&id).await.map_err(ShareCommandError::from),
        Err(error) => Err(error.into()),
    };
    remote::checkpoint(store, &session).await;
    outcome?;
    tracing::info!(opaque_id = %id, "share: link revoked");
    Ok(id)
}

/// Print what `capsule share create` registered: the count, the URL on a line of its own so it
/// can be copied or piped, how to revoke it, and what the recipient needs besides the URL.
pub fn render_issued(bundle: &Bundle, issued: &IssuedShare) {
    let count = [(
        "count",
        Value::Int(i64::try_from(issued.covered).unwrap_or(i64::MAX)),
    )];
    println!(
        "{}",
        bundle.format(keys::SHARE_CREATE_SUCCESS, &count).green()
    );
    println!("{}", issued.url);
    if issued.passphrase_protected {
        println!(
            "{}",
            bundle
                .format(keys::SHARE_CREATE_PASSPHRASE_NOTICE, &[])
                .yellow()
        );
    }
    if let Some(expires_at) = &issued.expires_at {
        println!(
            "{}",
            bundle.format(
                keys::SHARE_CREATE_EXPIRES_NOTICE,
                &[("expires_at", Value::Str(expires_at))]
            )
        );
    }
    println!(
        "{}",
        bundle
            .format(
                keys::SHARE_CREATE_REVOKE_HINT,
                &[("opaque_id", Value::Str(&issued.opaque_id))]
            )
            .cyan()
    );
}

/// A localized, human-readable reason for a `capsule share` failure.
#[must_use]
pub fn describe_error(bundle: &Bundle, error: &ShareCommandError) -> String {
    match error {
        ShareCommandError::InvalidExpiry(value) => bundle.format(
            keys::SHARE_CREATE_INVALID_EXPIRY,
            &[("value", Value::Str(value))],
        ),
        ShareCommandError::ExpiryNotInFuture(value) => bundle.format(
            keys::SHARE_CREATE_EXPIRY_NOT_IN_FUTURE,
            &[("value", Value::Str(value))],
        ),
        ShareCommandError::InvalidOpaqueId(value) => bundle.format(
            keys::SHARE_REVOKE_INVALID_OPAQUE_ID,
            &[("value", Value::Str(value))],
        ),
        ShareCommandError::EmptyScope => bundle.format(keys::SHARE_CREATE_EMPTY_SCOPE, &[]),
        ShareCommandError::NotHeld { missing } => bundle.format(
            keys::SHARE_CREATE_NOT_HELD,
            &[(
                "count",
                Value::Int(i64::try_from(missing.len()).unwrap_or(i64::MAX)),
            )],
        ),
        ShareCommandError::Sharing(SharingError::ScopeUnavailable) => {
            bundle.format(keys::SHARE_CREATE_SCOPE_UNAVAILABLE, &[])
        }
        ShareCommandError::Remote(RemoteError::NotAuthenticated) => {
            bundle.format(keys::AUTH_NOT_AUTHENTICATED, &[])
        }
        other => match other.error_code() {
            Some(code) => bundle.format(code, &[]),
            None => other.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(raw: &str) -> jiff::Timestamp {
        raw.parse().expect("a timestamp")
    }

    #[test]
    fn an_expiry_is_normalized_to_utc() {
        let now = at("2026-01-01T00:00:00Z");
        assert_eq!(
            normalize_expiry("2026-12-31T23:59:59+02:00", now).expect("valid"),
            "2026-12-31T21:59:59Z"
        );
        assert_eq!(
            normalize_expiry(" 2026-06-01T00:00:00Z\n", now).expect("valid"),
            "2026-06-01T00:00:00Z"
        );
    }

    #[test]
    fn an_expiry_that_does_not_parse_is_refused_naming_what_was_typed() {
        let now = at("2026-01-01T00:00:00Z");
        for raw in ["tomorrow", "2026-12-31", ""] {
            assert!(
                matches!(
                    normalize_expiry(raw, now),
                    Err(ShareCommandError::InvalidExpiry(ref typed)) if typed == raw
                ),
                "{raw:?} is not RFC 3339"
            );
        }
    }

    #[test]
    fn an_expiry_at_or_before_now_is_refused() {
        let now = at("2026-01-01T00:00:00Z");
        for raw in ["2026-01-01T00:00:00Z", "2025-12-31T23:59:59Z"] {
            assert!(matches!(
                normalize_expiry(raw, now),
                Err(ShareCommandError::ExpiryNotInFuture(_))
            ));
        }
    }

    #[test]
    fn an_opaque_id_is_32_lowercase_hex_characters() {
        let id = "0123456789abcdef0123456789abcdef";
        assert_eq!(validate_opaque_id(id).expect("valid"), id);
        assert_eq!(validate_opaque_id(&format!(" {id}\n")).expect("valid"), id);
        for raw in [
            "0123456789ABCDEF0123456789ABCDEF",
            "0123456789abcdef0123456789abcde",
            "0123456789abcdef0123456789abcdef0",
            "0123456789abcdef0123456789abcdeg",
            "https://capsule.example.com/s/0123456789abcdef0123456789abcdef",
        ] {
            assert!(
                matches!(
                    validate_opaque_id(raw),
                    Err(ShareCommandError::InvalidOpaqueId(_))
                ),
                "{raw:?} is not an opaque id"
            );
        }
    }

    #[test]
    fn the_metadata_record_is_never_reported_missing_and_every_unheld_ciphertext_is() {
        let record = Hash32([1; 32]);
        let held = Hash32([2; 32]);
        let unheld = Hash32([3; 32]);
        let on_server: HashSet<String> = [held.to_hex()].into_iter().collect();
        assert_eq!(
            missing_ciphertexts(&[record, held, unheld], &on_server),
            vec![unheld.to_hex()]
        );
        assert!(missing_ciphertexts(&[record, held], &on_server).is_empty());
        assert!(missing_ciphertexts(&[record], &HashSet::<String>::new()).is_empty());
    }

    #[test]
    fn every_local_refusal_is_described_from_the_catalog() {
        let bundle = Bundle::for_locale("en");
        let cases = [
            ShareCommandError::InvalidExpiry("tomorrow".to_owned()),
            ShareCommandError::ExpiryNotInFuture("2020-01-01T00:00:00Z".to_owned()),
            ShareCommandError::InvalidOpaqueId("nope".to_owned()),
            ShareCommandError::EmptyScope,
            ShareCommandError::NotHeld {
                missing: vec!["ab".to_owned(), "cd".to_owned()],
            },
            ShareCommandError::Sharing(SharingError::ScopeUnavailable),
            ShareCommandError::Remote(RemoteError::NotAuthenticated),
        ];
        for error in &cases {
            let described = describe_error(&bundle, error);
            assert!(
                !described.starts_with("cli.") && !described.is_empty(),
                "{error:?} renders a catalog message, not a key: {described}"
            );
        }
        assert!(describe_error(&bundle, &cases[0]).contains("tomorrow"));
        assert!(describe_error(&bundle, &cases[4]).contains('2'));
    }

    #[test]
    fn a_request_never_prints_its_link_passphrase() {
        let request = CreateRequest {
            scope: ShareScope::Asset(uuid::Uuid::nil()),
            expires_at: None,
            passphrase: Some("hunter2-correct-horse".to_owned()),
        };
        let printed = format!("{request:?}");
        assert!(!printed.contains("hunter2"), "{printed}");
        assert!(printed.contains("<redacted>"), "{printed}");
    }

    #[test]
    fn a_server_refusal_is_described_by_its_error_code() {
        let bundle = Bundle::for_locale("en");
        let error = ShareCommandError::Share(ShareError::Unavailable {
            code: Some("error.share.unavailable".to_owned()),
        });
        assert_eq!(error.error_code(), Some("error.share.unavailable"));
        assert_eq!(
            describe_error(&bundle, &error),
            bundle.format("error.share.unavailable", &[])
        );
    }
}
