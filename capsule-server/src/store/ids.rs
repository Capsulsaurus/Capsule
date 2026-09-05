//! The identifier newtypes the state ports speak in.
//!
//! Every one of these was a bare `&str` in the Salvo grab-bag, which is how a session id, a
//! user id and a hand-formatted `passkey_reg:{id}` key all became the same type at the storage
//! boundary. Distinct newtypes make a wrong-store call a compile error instead of a runtime
//! miss.
//!
//! Ids that are **secrets** — a revoke-all challenge, an enrollment code, a WebAuthn ceremony
//! id (which is what the browser cookie carries) — redact themselves in `Debug`, so no
//! `tracing` field or panic message can leak a live credential into a log.

use std::fmt;

/// Declares a plain, loggable identifier newtype.
macro_rules! plain_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            /// Wraps an already-validated identifier.
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// The identifier's text, for the backend key that carries it.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({:?})", stringify!($name), self.0)
            }
        }
    };
}

/// Declares an identifier newtype that is a bearer secret and must never print itself.
macro_rules! secret_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            /// Wraps an already-generated secret identifier.
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// The secret's text. Callers must not log the result.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        /// Redacted: this value is a bearer credential, and a `tracing` field or a panic
        /// message must not be able to publish one.
        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}(<redacted>)", stringify!($name))
            }
        }
    };
}

plain_id! {
    /// An account. The billing/namespace entity is [`OwnerId`]; this is the human.
    UserId
}

plain_id! {
    /// The billing and namespace entity an upload is accounted to.
    OwnerId
}

plain_id! {
    /// One open authentication session.
    SessionId
}

plain_id! {
    /// One in-flight upload session.
    UploadId
}

plain_id! {
    /// The asset row an upload session reserves at creation.
    AssetId
}

plain_id! {
    /// The album an upload is filed into.
    AlbumId
}

plain_id! {
    /// The device-enrollment relay channel opened when a code is redeemed.
    ///
    /// Not secret in the bearer sense — possession of the channel id alone relays only opaque
    /// ciphertext the server never decodes — but it is high-entropy and scoped to one
    /// ceremony, so it is kept its own type rather than a bare string.
    ChannelId
}

secret_id! {
    /// A single-use revoke-all challenge. Signed by the account's identity key and burned on
    /// the first attempt, successful or not.
    ChallengeToken
}

secret_id! {
    /// A device-enrollment code, in either of its two redeemable spellings — the full-entropy
    /// form the QR payload carries, and the shorter transcribable fallback.
    EnrollmentCode
}

secret_id! {
    /// The `state` an OIDC authorization request carries (slice `S-N1`).
    ///
    /// The key to one pending authorization: whoever presents it at the callback redeems the
    /// nonce and PKCE verifier it names, so it is a bearer credential for the length of the
    /// ceremony and is burned on the first presentation, successful or not.
    OidcState
}

secret_id! {
    /// The `nonce` an OIDC authorization request carries and the ID token must echo.
    ///
    /// Not a bearer secret in the strict sense — it travels in the authorization URL — but a
    /// predictable one would let a captured ID token be replayed against a fresh ceremony, so
    /// it is generated with the same entropy as the state and kept out of logs with it.
    OidcNonce
}

secret_id! {
    /// The PKCE `code_verifier` (RFC 7636) held server-side between the two legs of an OIDC
    /// authorization-code ceremony.
    ///
    /// The one value that turns an intercepted authorization code into nothing: the token
    /// endpoint refuses a code presented without the verifier its challenge was derived from.
    PkceVerifier
}

secret_id! {
    /// The authorization code an identity provider hands back through the client's redirect.
    ///
    /// Single-use at the provider, and worthless without the [`PkceVerifier`] — but a code in a
    /// log line is still half of a credential, so it redacts itself like the rest.
    AuthorizationCode
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_id_prints_its_value() {
        let id = SessionId::new("sid-1");
        assert_eq!(id.to_string(), "sid-1");
        assert_eq!(format!("{id:?}"), r#"SessionId("sid-1")"#);
    }

    #[test]
    fn a_secret_id_never_prints_its_value() {
        let token = ChallengeToken::new("super-secret-challenge");
        let rendered = format!("{token:?}");
        assert!(
            !rendered.contains("super-secret-challenge"),
            "a bearer secret must not reach a log through Debug, got {rendered}"
        );
        assert_eq!(rendered, "ChallengeToken(<redacted>)");
        assert_eq!(
            token.as_str(),
            "super-secret-challenge",
            "the value is still reachable for the backend key"
        );
    }

    #[test]
    fn ids_of_different_kinds_are_different_types() {
        // Compile-time property, asserted by construction: the two ids below hold the same
        // text but no `==` between them exists, so a session id cannot be passed where an
        // upload id is expected. If these ever unify, this file stopped doing its job.
        let session = SessionId::new("shared-text");
        let upload = UploadId::new("shared-text");
        assert_eq!(session.as_str(), upload.as_str());
    }
}
