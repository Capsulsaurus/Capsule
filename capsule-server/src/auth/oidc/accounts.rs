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
//! # The in-memory adapter holds its own rows
//!
//! [`InMemoryFederatedAccounts`] is the development profile's adapter and it does **not** share
//! rows with [`InMemoryAccounts`](crate::auth::InMemoryAccounts), the password directory. That is
//! a gap, recorded in issue #460 rather than papered over: under `serve --memory` an identity
//! whose address matches a password account is not refused, and an OIDC account has no profile
//! row. The Postgres adapter is written over one account table, where both properties hold.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError};

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

/// One federated account, as this adapter holds it.
#[derive(Debug, Clone)]
struct Row {
    user_id: UserId,
    #[allow(
        dead_code,
        reason = "held for the profile row the Postgres adapter will expose"
    )]
    created_at: Timestamp,
}

#[derive(Debug, Default)]
struct Held {
    /// `(issuer, subject)` → the account.
    links: BTreeMap<(String, String), Row>,
    /// Address → the account that asserted it first.
    addresses: BTreeMap<String, UserId>,
}

/// Federated accounts held in this process.
///
/// See the module docs for what it does and does not share with the password directory.
#[derive(Debug, Default)]
pub struct InMemoryFederatedAccounts {
    held: Mutex<Held>,
}

impl InMemoryFederatedAccounts {
    /// An empty directory.
    pub fn new() -> Self {
        Self::default()
    }

    /// How many federated accounts are held.
    pub fn len(&self) -> usize {
        self.held().links.len()
    }

    /// Whether none is held yet.
    pub fn is_empty(&self) -> bool {
        self.held().links.is_empty()
    }

    fn held(&self) -> MutexGuard<'_, Held> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl FederatedAccounts for InMemoryFederatedAccounts {
    fn resolve_or_create<'a>(
        &'a self,
        identity: &'a VerifiedIdentity,
        user: &'a UserId,
        at: Timestamp,
    ) -> DirectoryFuture<'a, FederatedLink> {
        Box::pin(async move {
            // One critical section, as the port requires.
            let mut held = self.held();
            let key = (identity.issuer.clone(), identity.subject.clone());
            if let Some(row) = held.links.get(&key) {
                return Ok(FederatedLink::Linked(row.user_id.clone()));
            }
            if let Some(email) = &identity.email
                && held.addresses.contains_key(email)
            {
                tracing::info!(issuer = %identity.issuer, "a federated sign-in asserted an address another account holds");
                return Ok(FederatedLink::AddressTaken);
            }
            if let Some(email) = &identity.email {
                held.addresses.insert(email.clone(), user.clone());
            }
            held.links.insert(
                key,
                Row {
                    user_id: user.clone(),
                    created_at: at,
                },
            );
            tracing::info!(%user, issuer = %identity.issuer, "created an account for a federated identity");
            Ok(FederatedLink::Created(user.clone()))
        })
    }
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

    #[tokio::test]
    async fn the_first_sign_in_creates_and_the_second_links_the_same_account() {
        let accounts = InMemoryFederatedAccounts::new();
        let first = accounts
            .resolve_or_create(
                &identity("sub-1", Some("a@example.test")),
                &user("1"),
                Timestamp::UNIX_EPOCH,
            )
            .await
            .expect("answers");
        assert_eq!(first, FederatedLink::Created(user("1")));

        // A different minted id: the existing link wins, and the new id is discarded.
        let second = accounts
            .resolve_or_create(
                &identity("sub-1", Some("a@example.test")),
                &user("2"),
                Timestamp::UNIX_EPOCH,
            )
            .await
            .expect("answers");
        assert_eq!(second, FederatedLink::Linked(user("1")));
        assert_eq!(accounts.len(), 1);
    }

    #[tokio::test]
    async fn the_same_subject_at_another_issuer_is_another_person() {
        let accounts = InMemoryFederatedAccounts::new();
        accounts
            .resolve_or_create(&identity("sub-1", None), &user("1"), Timestamp::UNIX_EPOCH)
            .await
            .expect("answers");
        let mut elsewhere = identity("sub-1", None);
        elsewhere.issuer = "https://other.example.test".to_owned();
        assert_eq!(
            accounts
                .resolve_or_create(&elsewhere, &user("2"), Timestamp::UNIX_EPOCH)
                .await
                .expect("answers"),
            FederatedLink::Created(user("2"))
        );
    }

    #[tokio::test]
    async fn an_asserted_address_another_account_holds_is_refused_and_nothing_is_written() {
        let accounts = InMemoryFederatedAccounts::new();
        accounts
            .resolve_or_create(
                &identity("sub-1", Some("a@example.test")),
                &user("1"),
                Timestamp::UNIX_EPOCH,
            )
            .await
            .expect("answers");
        assert_eq!(
            accounts
                .resolve_or_create(
                    &identity("sub-2", Some("a@example.test")),
                    &user("2"),
                    Timestamp::UNIX_EPOCH,
                )
                .await
                .expect("answers"),
            FederatedLink::AddressTaken
        );
        assert_eq!(accounts.len(), 1, "the refused identity created nothing");
        // And it is still refused, rather than having been half-linked.
        assert_eq!(
            accounts
                .resolve_or_create(
                    &identity("sub-2", Some("a@example.test")),
                    &user("3"),
                    Timestamp::UNIX_EPOCH,
                )
                .await
                .expect("answers"),
            FederatedLink::AddressTaken
        );
    }

    #[tokio::test]
    async fn an_identity_without_an_address_is_an_account_too() {
        let accounts = InMemoryFederatedAccounts::new();
        assert_eq!(
            accounts
                .resolve_or_create(&identity("sub-1", None), &user("1"), Timestamp::UNIX_EPOCH)
                .await
                .expect("answers"),
            FederatedLink::Created(user("1"))
        );
        assert_eq!(
            accounts
                .resolve_or_create(&identity("sub-2", None), &user("2"), Timestamp::UNIX_EPOCH)
                .await
                .expect("answers"),
            FederatedLink::Created(user("2")),
            "two addressless identities do not collide on the absent address"
        );
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
