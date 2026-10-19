//! [`FederatedAccounts`] — which Capsule account a verified identity is.
//!
//! # One method, one atomic operation
//!
//! `resolve_or_create` is keyed on `(issuer, subject)`: the pair OpenID Connect Core §2 makes
//! stable for a person at a provider. It looks the pair up and, absent, creates the account — in
//! **one** operation, for the reason [`AccountRegistry::create`](crate::auth::AccountRegistry)
//! is one: a caller that read, saw nothing and then wrote has a window in which a second callback
//! for the same person lands, and both would believe they created the account.
//!
//! # Not a method on [`AccountRegistry`](crate::auth::AccountRegistry)
//!
//! That port's own docs forbid it — *"a port with a second method is a port that will have
//! six"* — and the two are different operations besides: `create` takes a password it hashes,
//! and an account created here must have **none**. The adapter contract states it as an
//! invariant rather than a convention: an account row whose credential is null makes
//! [`AccountDirectory::authenticate`](crate::auth::AccountDirectory) return `Refused`, never
//! `Granted`. No password an attacker could guess exists for an OIDC account, because no
//! password exists.
//!
//! # No linking by address
//!
//! An unknown `(issuer, subject)` whose asserted `email` already belongs to an account answers
//! [`FederatedLink::AddressTaken`], which the route renders as `409 error.auth.oidc_address_taken`
//! — never a link. The address is a claim the provider controls; honouring it as a link key would
//! hand the matching local account to anyone who can set an email at the provider. The disclosure
//! the `409` makes is the one `error.auth.user_already_exists` already makes at registration, so
//! it adds no new oracle. Deliberately linking an existing account to a provider identity is a
//! separate, authenticated ceremony, and is out of scope.
//!
//! # Only a verified address is reserved, or compared
//!
//! An address the provider asserts **without** `email_verified` is one anybody at that provider
//! could have typed. Reserving it would let a person register an unverified address at the
//! provider and thereby block the address's real owner from ever signing in here — a targeted
//! denial of service costing one sign-up — so an unverified address is carried on the identity
//! and otherwise ignored: it reserves nothing and collides with nothing. [`reserved_address`] is
//! that rule, and both adapters read the claim through it and nowhere else.
//!
//! The comparison is against **every** address the account table holds: the one each password
//! account registered with, and the verified one each earlier federated account recorded. The
//! verified-only rule applies to the side that is asserting (decision 11 of #459, kept by #460).
//!
//! # One account table, two adapters
//!
//! Both adapters implement this port on the object that implements the four password ports,
//! over the same rows: [`InMemoryAccounts`](crate::auth::InMemoryAccounts) for the development
//! profile and [`PostgresAccounts`](crate::auth::PostgresAccounts) for the durable one. That is
//! what makes the `409` cross-directory, what gives a federated account a profile row, and what
//! lets [`crate::auth::conformance`] assert both properties — and the null-credential invariant
//! above — against both adapters with one suite.
//!
//! A federated account whose provider asserted no verified address still has a profile: both
//! adapters answer it with [`ProfileRecord::email`](crate::auth::ProfileRecord) `None`, and
//! `GET /v1/auth/profile` serves it with the `email` key absent (#603).

use std::fmt;

use jiff::Timestamp;

use super::claims::VerifiedIdentity;
use super::provider::Disabled;
use crate::auth::{DirectoryError, DirectoryFuture};
use crate::store::UserId;

/// What resolving an identity did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FederatedLink {
    /// The pair was known, and this is the account it names.
    Linked(UserId),
    /// The pair was new; an account was created under the id the caller minted.
    Created(UserId),
    /// The pair was new and its asserted address already belongs to an account. Nothing written.
    AddressTaken,
}

/// Which account a verified provider identity is.
pub trait FederatedAccounts: fmt::Debug + Send + Sync {
    /// The account for `identity`, created under `user` at `at` if it does not exist.
    ///
    /// The id is minted **above** this port for the reason `AccountRegistry::create` takes one:
    /// it is a fact about the server's clock rather than about the backend.
    fn resolve_or_create<'a>(
        &'a self,
        identity: &'a VerifiedIdentity,
        user: &'a UserId,
        at: Timestamp,
    ) -> DirectoryFuture<'a, FederatedLink>;
}

impl FederatedAccounts for Disabled {
    fn resolve_or_create<'a>(
        &'a self,
        _identity: &'a VerifiedIdentity,
        _user: &'a UserId,
        _at: Timestamp,
    ) -> DirectoryFuture<'a, FederatedLink> {
        Box::pin(async {
            Err(DirectoryError::Unavailable {
                detail: "no identity provider is configured".to_owned(),
            })
        })
    }
}

/// The address `identity` reserves and is compared by: its `email`, only when the provider
/// asserted it verified (module docs). Every adapter reads the claim through this and nowhere
/// else, so the rule cannot be applied in one and forgotten in the other.
pub fn reserved_address(identity: &VerifiedIdentity) -> Option<&str> {
    identity
        .email
        .as_deref()
        .filter(|_| identity.email_verified)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(subject: &str, email: Option<&str>) -> VerifiedIdentity {
        VerifiedIdentity {
            issuer: "https://idp.example.test".to_owned(),
            subject: subject.to_owned(),
            email: email.map(str::to_owned),
            email_verified: true,
        }
    }

    fn user(tag: &str) -> UserId {
        UserId::new(format!("user-{tag}"))
    }

    // What each adapter does with an identity is asserted once, against both adapters, by the
    // shared suite in `crate::auth::conformance`. What is left here is the rule itself and the
    // null object.

    #[test]
    fn only_a_verified_address_is_reserved() {
        assert_eq!(
            reserved_address(&identity("sub-1", Some("a@example.test"))),
            Some("a@example.test")
        );
        let mut unverified = identity("sub-1", Some("a@example.test"));
        unverified.email_verified = false;
        assert_eq!(reserved_address(&unverified), None);
        assert_eq!(reserved_address(&identity("sub-1", None)), None);
    }

    #[tokio::test]
    async fn the_disabled_directory_refuses() {
        assert!(
            Disabled
                .resolve_or_create(&identity("sub-1", None), &user("1"), Timestamp::UNIX_EPOCH)
                .await
                .is_err()
        );
    }
}
