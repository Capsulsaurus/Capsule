//! The OpenID Connect relying party (slice `S-N1`).
//!
//! # What it is, and what it is not
//!
//! Capsule is a **relying party only** (design/authentication.md, "Choosing an Auth Path"): an
//! external identity provider authenticates the *session*, and the master key never derives
//! from, and is never visible to, whatever the provider verified. Account lifecycle policy lives
//! at the provider. What this module does is the authorization-code + PKCE handshake, the checks
//! on what comes back, and the mapping from a provider's stable subject to a Capsule account —
//! after which the session is opened by exactly the function the password path uses.
//!
//! # Shape
//!
//! | Concern | Lives | Why |
//! | --- | --- | --- |
//! | Every check an ID token must pass | [`claims`] | a pure function, so every negative case is a unit test with no socket and no clock |
//! | Provider metadata and its cache | `discovery` | one fetch a day, refused if it names a different issuer |
//! | The provider's signing keys | `jwks` | refetched on an unknown `kid`, at most once a minute |
//! | The port the routes drive, and its one HTTP adapter | `provider` | the only external boundary the feature has, so the only thing doubled |
//! | Which account a verified identity is | `accounts` | one atomic operation keyed on `(issuer, subject)` |
//! | The pending ceremony between the two legs | [`crate::store::OidcAuthorizationStore`] | a single-use, short-window credential, which is what the ceremony stores are for |
//!
//! # Hand-written over `jsonwebtoken`, not `openidconnect`
//!
//! `openidconnect` would have pulled in `chrono` and the `log` facade — both banned — and
//! `rsa 0.9` carrying RUSTSEC-2023-0071 with no fixed release, which `deny.toml` would not catch
//! because only the licence check is wired. The hard part, verifying a signature against a JWKS,
//! is already in the workspace's JWT crate; what is left is discovery, a form `POST` and the
//! claim checks, each of which is a security decision this repository wants written where a
//! reader can see it. See the OIDC row in design/dependencies.md.

pub mod claims;

pub use self::claims::{
    ALLOWED_ALGORITHMS, CLOCK_SKEW_SECONDS, ClaimRejection, Expectations, MAX_SUBJECT_LENGTH,
    VerifiedIdentity, verify_id_token,
};
