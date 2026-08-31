//! The server-scoped public record (`S-C18`) — `server-info` and `deprecation`.
//!
//! # What may be here, and the one rule that governs it
//!
//! The registry's rule is a single sentence in design/authentication.md and it is the reason
//! this module is small: **`.well-known/` never enumerates the user list.** A federated peer
//! that can list every account on a server has an abuse surface (harassment-target discovery,
//! account enumeration) that no amount of rate limiting takes back. So everything published
//! from here is server-scoped by construction — endpoints, a window, a public key, a date — and
//! nothing in this module can name a user, because nothing in it holds a user.
//!
//! That is also why `moved/{user}` is not here. It is the one registry record that names an
//! account, and it is admissible only because the *user* signs it and the *user* initiates the
//! migration. It is post-v1 with Account Portability, and adding it is a decision about that
//! exception rather than another row.
//!
//! # The signing key is the one that signs
//!
//! `server-info` publishes the server's operational Ed25519 key: classical only, per the
//! [operational-signature carve-out](design/cryptography/primitives.md), and the key a
//! federation capability token is verified against offline by the peer holding it. Today the
//! same key signs this server's own session tokens, which is not a coincidence — the operator
//! supplies one `JWT_ED25519_DER` and design/guides/self-hosting.md derives the rest from it.
//!
//! So the published key is read **out of the token signer** rather than configured beside it
//! ([`SessionTokens::public_key`](crate::auth::SessionTokens::public_key)). The alternative —
//! an operator pasting a public key into a config file next to the private one — publishes a
//! key that is *usually* the signing key, and the failure when it is not is silent on this side
//! and total on the peer's: every token this server minted becomes unverifiable at once, which
//! from outside is indistinguishable from a compromise. It is the same invariant
//! [`AttestationContext`](crate::attestation::AttestationContext) holds for the attestation
//! key, for the same reason, and the two keys stay distinct: different lifetimes, different
//! blast radii.
//!
//! # Deprecation is announced, not merely applied
//!
//! Dropping a `protocol_version` is a breaking change, and design/threat-model/schema-rules.md
//! requires the cutoff be published **at least an announcement window ahead** — 90 days by
//! default, deployment-configurable. That is enforced here at construction
//! ([`ServerInfo::announce`]) rather than checked by a reviewer, because a server that
//! published a cutoff for next Tuesday would be conforming to the letter of the record's shape
//! while breaking the promise the record exists to make.
//!
//! An announcement is not the cutoff taking effect. The window in [`ServerInfo::protocol`] is
//! what the server accepts *today*; the announcement says what it will accept later. Moving the
//! window is an operator action on the cutoff date, and this module deliberately does not do it
//! on a timer — a server that silently narrowed its own accepted range at midnight would be
//! rejecting writes for a reason no log line names.

pub mod revocation;

use std::sync::Arc;

use jiff::{SignedDuration, Timestamp};

/// How far ahead of a cutoff a deprecation must be announced, absent an operator override.
///
/// Ninety days, from design/threat-model/schema-rules.md. Deployment-configurable through
/// [`ServerInfo::with_announcement_window`], which is what makes this a default rather than a
/// constant the policy is stated in.
pub const DEFAULT_ANNOUNCEMENT_WINDOW: SignedDuration = SignedDuration::from_hours(90 * 24);

/// Where a client performs the auth ceremony.
///
/// Published rather than left for a client to assemble from [`ServerInfo::api_base_url`],
/// because a client that hardcodes `{base}/auth/login` has silently pinned this server's route
/// layout and will break on any deployment that mounts it elsewhere — which is exactly what a
/// discovery record exists to prevent.
///
/// The suffixes are literals here and literals again in
/// [`crate::routes::auth`], which is a duplication with no type to bind the two together. What
/// keeps it honest is a test that posts to the *published* login URL and asserts the server
/// answers it at all: a renamed route makes that a `404` rather than a silent lie in a record
/// nobody fetches during development.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthEndpoints {
    /// Where a session is opened.
    pub login: String,
    /// Where an access token is rotated.
    pub refresh: String,
    /// Where a session is ended.
    pub logout: String,
}

impl AuthEndpoints {
    /// The ceremony's three URLs, under `api_base_url`.
    fn under(api_base_url: &str) -> Self {
        let base = api_base_url.trim_end_matches('/');
        Self {
            login: format!("{base}/auth/login"),
            refresh: format!("{base}/auth/refresh"),
            logout: format!("{base}/auth/logout"),
        }
    }
}

/// The `protocol_version` range this server accepts.
///
/// Both ends are inclusive, and both are published: a client that knows only the maximum cannot
/// tell whether the version it is pinned to is still accepted, which is the question the
/// deprecation policy is entirely about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolWindow {
    /// The oldest `protocol_version` still accepted for writes.
    pub min: String,
    /// The newest this server speaks — the version it stamps on an album it provisions.
    pub max: String,
}

/// A published intention to stop accepting a `protocol_version`.
///
/// Carries its own announcement time rather than deriving one, so a record fetched today says
/// when it was first made and a client can see the window was honored rather than trusting it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeprecationAnnouncement {
    /// The lowest `protocol_version` that will remain accepted after the cutoff.
    pub min_protocol_version: String,
    /// When the announcement was first published.
    pub announced_at: Timestamp,
    /// When the versions below `min_protocol_version` stop being accepted for writes.
    pub cutoff: Timestamp,
    /// Where a human reads what to do about it.
    pub detail_url: Option<String>,
}

/// Why an announcement was refused.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AnnouncementError {
    /// The cutoff is sooner than the announcement window allows.
    ///
    /// Refused rather than clamped: silently moving an operator's cutoff date is a policy
    /// decision made by a library, and the operator would learn about it from a client's bug
    /// report.
    #[error(
        "a deprecation cutoff at {cutoff} is {lead_days} days out, inside the {window_days}-day announcement window"
    )]
    InsideWindow {
        /// The cutoff that was refused.
        cutoff: Timestamp,
        /// How much notice it actually gave.
        lead_days: i64,
        /// How much it needed to give.
        window_days: i64,
    },
    /// The cutoff has already passed.
    #[error("a deprecation cutoff at {cutoff} is already in the past")]
    AlreadyPassed {
        /// The cutoff that was refused.
        cutoff: Timestamp,
    },
}

/// The public, server-scoped facts a client or peer can learn without an account.
#[derive(Debug, Clone)]
pub struct ServerInfo {
    server_id: String,
    api_base_url: String,
    auth: AuthEndpoints,
    federation_url: Option<String>,
    protocol: ProtocolWindow,
    signing_key: Vec<u8>,
    announcement_window: SignedDuration,
    deprecations: Vec<DeprecationAnnouncement>,
}

impl ServerInfo {
    /// The facts a deployment always has: who it is, where its API is, what it speaks, and the
    /// key its tokens verify under.
    ///
    /// `signing_key` is the raw Ed25519 public key, and callers are expected to read it from
    /// the signer rather than to hold a copy — see this module's documentation.
    pub fn new(
        server_id: impl Into<String>,
        api_base_url: impl Into<String>,
        protocol: ProtocolWindow,
        signing_key: Vec<u8>,
    ) -> Self {
        let api_base_url = api_base_url.into();
        Self {
            server_id: server_id.into(),
            auth: AuthEndpoints::under(&api_base_url),
            api_base_url,
            federation_url: None,
            protocol,
            signing_key,
            announcement_window: DEFAULT_ANNOUNCEMENT_WINDOW,
            deprecations: Vec::new(),
        }
    }

    /// Declare that this deployment federates, and where.
    ///
    /// Absent by default. A server that publishes a federation endpoint it does not serve
    /// invites peers to fail against it, and "federation is off" is a legitimate and probably
    /// common deployment — so the absence is the record's way of saying so, rather than a flag
    /// beside a URL that is a placeholder.
    #[must_use]
    pub fn with_federation(mut self, url: impl Into<String>) -> Self {
        self.federation_url = Some(url.into());
        self
    }

    /// Override the announcement window this server holds itself to.
    #[must_use]
    pub fn with_announcement_window(mut self, window: SignedDuration) -> Self {
        self.announcement_window = window;
        self
    }

    /// Publish a deprecation cutoff, refusing one that gives less notice than the window.
    ///
    /// # Errors
    ///
    /// Returns [`AnnouncementError::AlreadyPassed`] if the cutoff is not in the future, and
    /// [`AnnouncementError::InsideWindow`] if it is sooner than the announcement window.
    pub fn announce(
        &mut self,
        announcement: DeprecationAnnouncement,
    ) -> Result<(), AnnouncementError> {
        let cutoff = announcement.cutoff;
        if cutoff <= announcement.announced_at {
            return Err(AnnouncementError::AlreadyPassed { cutoff });
        }

        let earliest = crate::store::deadline(announcement.announced_at, self.announcement_window);
        if cutoff < earliest {
            let lead = cutoff.duration_since(announcement.announced_at);
            let window = earliest.duration_since(announcement.announced_at);
            return Err(AnnouncementError::InsideWindow {
                cutoff,
                lead_days: lead.as_hours() / 24,
                window_days: window.as_hours() / 24,
            });
        }

        tracing::info!(
            min_protocol_version = %announcement.min_protocol_version,
            cutoff = %cutoff,
            "a protocol deprecation was announced"
        );
        self.deprecations.push(announcement);
        Ok(())
    }

    /// This server's canonical origin.
    pub fn server_id(&self) -> &str {
        &self.server_id
    }

    /// Where the versioned API lives.
    pub fn api_base_url(&self) -> &str {
        &self.api_base_url
    }

    /// Where a client performs the auth ceremony.
    pub fn auth(&self) -> &AuthEndpoints {
        &self.auth
    }

    /// Where federated peers talk to this server, if it federates at all.
    pub fn federation_url(&self) -> Option<&str> {
        self.federation_url.as_deref()
    }

    /// The `protocol_version` range accepted today.
    pub fn protocol(&self) -> &ProtocolWindow {
        &self.protocol
    }

    /// The raw Ed25519 public key this server's tokens verify under.
    pub fn signing_key(&self) -> &[u8] {
        &self.signing_key
    }

    /// Every published deprecation, in announcement order.
    pub fn deprecations(&self) -> &[DeprecationAnnouncement] {
        &self.deprecations
    }
}

/// The discovery module's collaborators.
///
/// [`ServerInfo`] is a value rather than a port: it is deployment configuration read from the
/// process's own environment, not state a database owns. The revocation list *is* a port,
/// because it is written by every revocation and read by every peer.
#[derive(Debug, Clone)]
pub struct DiscoveryContext {
    info: Arc<ServerInfo>,
    revocations: Arc<dyn revocation::RevocationList>,
}

impl DiscoveryContext {
    /// Assembles the module.
    pub fn new(info: Arc<ServerInfo>, revocations: Arc<dyn revocation::RevocationList>) -> Self {
        Self { info, revocations }
    }

    /// The published server-scoped facts.
    pub fn info(&self) -> &ServerInfo {
        &self.info
    }

    /// The federation capability revocation list.
    pub fn revocations(&self) -> &dyn revocation::RevocationList {
        self.revocations.as_ref()
    }
}

#[cfg(test)]
mod tests;
