//! [`AccountProfiles`] and [`PasswordChange`] — the two mutable facts about an account that are
//! not its existence (slice `S-C54`).
//!
//! # Why these are two ports and not one, and neither is a method on the directory
//!
//! [`AccountDirectory`](super::AccountDirectory) answers one question — *is this person who they
//! say they are* — and its own docs say what the alternative would have been: *"a directory that
//! also listed accounts, created them or changed passwords would be the grab-bag `S-C29`
//! deleted, rebuilt one method at a time."* `S-C53` took creation out to its own port on exactly
//! that reasoning. This slice takes the remaining two.
//!
//! They are separate from each other for a sharper reason than tidiness. Reading and editing a
//! display name is an ordinary authenticated write: if it fails, a person's name is stale.
//! Changing a password is a **credential rotation** — it re-authenticates, it ends every other
//! session, and getting it wrong hands an account to whoever holds a stolen token. Behind one
//! trait, the two would share an adapter, a failure type and eventually a caller; a
//! `set_password` reachable from the routine profile write is a `set_password` somebody
//! eventually calls without the verification in front of it.
//!
//! # Three things this surface deliberately cannot do
//!
//! **It cannot change the login address.** The Salvo `POST /v1/auth/profile` could, with no
//! proof that the caller controls the new address and no mail path in the deployment to obtain
//! one. That is not a profile edit — it is the first step of an account takeover: a live token
//! moves the account onto an address the attacker owns, and every later recovery flow then
//! addresses them. The address is fixed at registration until there is a way to prove control of
//! a new one, and this port has no method for it rather than a method that refuses.
//!
//! **It cannot reset a forgotten password.** `S-C53` recorded that verdict for the retired
//! `/v1/auth/password-reset` pair and it stands: on an end-to-end-encrypted account a
//! server-issued reset is not a recovery, because the server cannot re-wrap a master key it has
//! never seen. The recovery story is the escrow blob (`S-C12`). What *this* port offers is a
//! password **change**, which is a different operation — it is authenticated by the password
//! being replaced.
//!
//! **It cannot read a password hash.** Same contract as the directory: the credential never
//! rises above the adapter. [`PasswordChange::set_password`] takes the new password in the clear
//! and owns hashing it, so Argon2id's parameters stay in one place.
//!
//! # No adapter here
//!
//! For the reason [`AccountDirectory`](super::AccountDirectory) and
//! [`AccountRegistry`](super::AccountRegistry) have none: the real one is Postgres, the test one
//! is a double, and a double in `src/` is a fake account store shipped inside the server binary.
//! The suite's lives in `tests/support/`.

use std::fmt;

use jiff::Timestamp;

use super::directory::DirectoryFuture;
use crate::store::UserId;

/// The longest display name this server will store.
///
/// A bound rather than a validation. The name is never parsed, never authorizes anything and is
/// rendered by clients that will truncate it long before this; what the ceiling is for is
/// refusing a field that is being used as storage. Generous on purpose — a tight limit would
/// exclude legitimate names in scripts where a "short" name is many code points.
pub const MAX_DISPLAY_NAME_CHARS: usize = 128;

/// What the server knows about an account, and is willing to tell that account.
///
/// Deliberately four fields. Everything a server stores about a person is a thing it can leak,
/// be compelled to produce, or get wrong, and this list is the whole of it — there is no phone
/// number, no locale, no avatar, and none of them is coming without a slice that argues for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileRecord {
    /// The account's identifier — the same one every manifest and every session names.
    pub user_id: UserId,
    /// The address the account signs in with. Fixed at registration; see the module docs.
    pub email: String,
    /// The name the account chose to be shown as, if it chose one.
    ///
    /// `None` and an empty string are the same state and the port stores the former: a client
    /// rendering "" would draw an empty label where it should fall back to the address.
    pub display_name: Option<String>,
    /// When the account was created.
    pub created_at: Timestamp,
}

/// A requested change to a profile.
///
/// One field, and it is a *nested* option on purpose. `None` means the caller did not mention
/// the display name and it must not change; `Some(None)` means the caller asked for it to be
/// cleared. A flat `Option<String>` cannot tell those apart, which is how a partial update
/// silently wipes a field the caller never sent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileUpdate {
    /// The display name to set, clear, or leave alone.
    pub display_name: Option<Option<String>>,
}

impl ProfileUpdate {
    /// Whether this asks for anything at all.
    ///
    /// An empty update is not an error — it is a no-op the route answers with the current
    /// profile — but the port is told, so an adapter can skip a write rather than issue an
    /// `UPDATE` that sets every column to itself.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.display_name.is_none()
    }
}

/// Why a display name was refused.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MalformedProfile {
    /// Past [`MAX_DISPLAY_NAME_CHARS`].
    #[error(
        "a display name may be at most {MAX_DISPLAY_NAME_CHARS} characters, and that is {chars}"
    )]
    DisplayNameTooLong {
        /// How long it was, in characters rather than bytes — the unit a person counts in.
        chars: usize,
    },
    /// Contains a control character.
    ///
    /// Refused rather than stripped: a name carrying a newline or a bidirectional override is
    /// one that renders as something other than itself in somebody else's client, and silently
    /// rewriting what a person typed is worse than telling them it will not do.
    #[error("a display name may not contain control characters")]
    DisplayNameControlCharacters,
}

/// Normalize a submitted display name into what the port stores.
///
/// Trims, maps the empty result onto `None`, and checks the two things the server is entitled to
/// check. It is a free function rather than a method so the route can run it *before* reaching
/// for a store, which is what keeps a malformed name from costing a database round trip.
///
/// # Errors
///
/// Returns [`MalformedProfile`] for a name past the ceiling or carrying control characters.
pub fn admissible_display_name(name: &str) -> Result<Option<String>, MalformedProfile> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let chars = trimmed.chars().count();
    if chars > MAX_DISPLAY_NAME_CHARS {
        return Err(MalformedProfile::DisplayNameTooLong { chars });
    }
    if trimmed.chars().any(char::is_control) {
        return Err(MalformedProfile::DisplayNameControlCharacters);
    }
    Ok(Some(trimmed.to_owned()))
}

/// Reading and editing the facts an account keeps about itself.
pub trait AccountProfiles: fmt::Debug + Send + Sync {
    /// The profile of `user`, or `None` when the directory holds no such account.
    ///
    /// `None` is reachable with a perfectly valid credential: a session outlives the account row
    /// it names if the account is deleted while a token is live. The route answers `404` rather
    /// than `500`, because the server is working correctly and the account is gone.
    fn read<'a>(&'a self, user: &'a UserId) -> DirectoryFuture<'a, Option<ProfileRecord>>;

    /// Apply `update` to `user` and return the profile as it now stands.
    ///
    /// **Read-modify-write is the adapter's problem, in one operation.** A caller that read,
    /// changed a field and wrote the whole record back would clobber a concurrent edit from
    /// another device — which is the same hazard [`AccountRegistry::create`] is one operation
    /// for, in a place where the loss is a name rather than an account.
    ///
    /// Returns `None` when the account does not exist, for the reason [`Self::read`] does.
    fn update<'a>(
        &'a self,
        user: &'a UserId,
        update: &'a ProfileUpdate,
    ) -> DirectoryFuture<'a, Option<ProfileRecord>>;
}

/// What replacing a password did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasswordChanged {
    /// The account's password is now the new one.
    Yes,
    /// There is no such account, and nothing was written.
    NoSuchAccount,
}

/// Replacing the password an account's sessions are opened with.
///
/// One method, and it does **not** verify the old password. Verification is
/// [`AccountDirectory::authenticate_user`](super::AccountDirectory::authenticate_user) — the
/// same code path a sign-in takes, with the same constant-time comparison and the same
/// failed-attempt bookkeeping — and reimplementing it here would be a second answer to the one
/// question the directory exists to answer. What that costs is a two-step route, and what it
/// buys is that a lockout applies to password changes exactly as it applies to sign-ins.
pub trait PasswordChange: fmt::Debug + Send + Sync {
    /// Replace `user`'s password with `password`.
    ///
    /// `password` is borrowed and must not be retained, logged, or included in any error. The
    /// adapter owns hashing end to end, for the reason the directory owns verification.
    ///
    /// The adapter must also **clear any lockout state** it holds for the account: a password
    /// change is a successful credential presentation, and leaving the failure count behind
    /// would lock a person out of an account they just proved they own.
    fn set_password<'a>(
        &'a self,
        user: &'a UserId,
        password: &'a str,
        at: Timestamp,
    ) -> DirectoryFuture<'a, PasswordChanged>;
}

#[cfg(test)]
mod tests {
    use super::{MAX_DISPLAY_NAME_CHARS, MalformedProfile, ProfileUpdate, admissible_display_name};

    #[test]
    fn a_blank_display_name_is_the_absent_one() {
        // The two states a client can express for "no name" collapse to one before storage, so
        // a client rendering the stored value never has to special-case "".
        assert_eq!(admissible_display_name("   "), Ok(None));
        assert_eq!(admissible_display_name(""), Ok(None));
    }

    #[test]
    fn a_display_name_is_trimmed_and_kept_verbatim_otherwise() {
        assert_eq!(
            admissible_display_name("  Ada Lovelace  "),
            Ok(Some("Ada Lovelace".to_owned()))
        );
    }

    #[test]
    fn the_ceiling_counts_characters_and_not_bytes() {
        // The unit a person counts in. A byte ceiling would refuse a name in a script where
        // every character is three or four bytes while accepting a much longer ASCII one.
        let name = "\u{1F600}".repeat(MAX_DISPLAY_NAME_CHARS);
        assert!(admissible_display_name(&name).is_ok());
        let over = "\u{1F600}".repeat(MAX_DISPLAY_NAME_CHARS + 1);
        assert_eq!(
            admissible_display_name(&over),
            Err(MalformedProfile::DisplayNameTooLong {
                chars: MAX_DISPLAY_NAME_CHARS + 1
            })
        );
    }

    #[test]
    fn a_control_character_is_refused_and_not_stripped() {
        assert_eq!(
            admissible_display_name("Ada\nLovelace"),
            Err(MalformedProfile::DisplayNameControlCharacters)
        );
    }

    #[test]
    fn an_absent_field_and_a_cleared_one_are_different_updates() {
        // The whole reason the field is a nested option: a flat one cannot express "leave it
        // alone", so every partial update would clear the name nobody mentioned.
        assert!(ProfileUpdate::default().is_empty());
        assert!(
            !ProfileUpdate {
                display_name: Some(None)
            }
            .is_empty()
        );
    }
}
