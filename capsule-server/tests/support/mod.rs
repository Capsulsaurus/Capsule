//! The doubles the auth suite drives the server with, and the fixture that assembles them.
//!
//! # Why the doubles live here and not in `src/`
//!
//! `S-C29` put its in-memory [`AuthStateStore`] adapter in `src/`, because a shared conformance
//! suite has to be runnable against an adapter written in another crate. Nothing here has that
//! constraint, and one of these types is a **credential directory that accepts whatever password
//! it was told to accept**. That belongs in a test binary and nowhere a server could link it.
//!
//! The [`SessionTokens`] signer is *not* doubled: it is the real one over a generated key, so
//! every 401 in the suite is produced by a token that genuinely does not verify.
//!
//! # Why both collaborators can be broken on demand rather than replaced
//!
//! `assert_declared_responses_covered` walks the whole document against **one** client's
//! recording, so every response the description promises — the two 500s included — has to be
//! produced by one server. A second fixture with a permanently broken store could not
//! contribute to that walk. So the store and the directory each carry a switch, and the
//! coverage test breaks them for one request and repairs them.

#![allow(
    dead_code,
    reason = "each test binary uses a different part of the fixture"
)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use capsule_server::App;
use capsule_server::auth::{
    AccountDirectory, Authentication, DirectoryError, DirectoryFuture, SessionTokens,
};
use capsule_server::store::memory::{InMemoryAuthState, ManualClock};
use capsule_server::store::{
    AuthStateStore, SessionId, SessionRecord, StoreError, StoreFuture, UserId,
};
use jiff::{SignedDuration, Timestamp};
use jsonwebtoken::{DecodingKey, EncodingKey};
use kynos::test::TestClient;

/// The session lifetime the fixture's store is built with.
///
/// Seven days, matching the refresh-token lifetime the Salvo deployment configured, so the suite
/// exercises a realistic "the refresh token dies with its record" window rather than a shorter
/// one that would hide the arrangement.
pub(crate) const SESSION_TTL: SignedDuration = SignedDuration::from_hours(24 * 7);

/// The account [`Fixture::working`] seeds.
pub(crate) const EMAIL: &str = "somebody@example.test";

/// The password that account authenticates with.
pub(crate) const PASSWORD: &str = "correct horse battery staple";

/// What a broken collaborator says. Asserted against, so a 500 body that leaked it would fail.
const REFUSAL: &str = "the double refuses on purpose";

// ===========================================================================================
// Account directory double
// ===========================================================================================

/// An account directory holding whatever the test told it.
///
/// Passwords are compared verbatim: hashing them would be a test of `argon2`, which is the real
/// adapter's business rather than this port's contract. What *is* the contract — three outcomes,
/// and a credential that never rises above the port — is exercised exactly as it would be
/// against Postgres.
#[derive(Debug, Default)]
pub(crate) struct InMemoryAccounts {
    accounts: Mutex<BTreeMap<String, Account>>,
    unavailable: AtomicBool,
}

#[derive(Debug, Clone)]
struct Account {
    user_id: UserId,
    password: String,
    locked: bool,
}

impl InMemoryAccounts {
    /// An empty directory.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Record an account that will authenticate with `password`.
    pub(crate) fn insert(&self, email: &str, password: &str, user_id: &UserId) {
        self.accounts().insert(
            email.to_owned(),
            Account {
                user_id: user_id.clone(),
                password: password.to_owned(),
                locked: false,
            },
        );
    }

    /// Put an existing account into the locked-out state.
    pub(crate) fn lock(&self, email: &str) {
        if let Some(account) = self.accounts().get_mut(email) {
            account.locked = true;
        }
    }

    /// Make every subsequent lookup fail, or stop.
    pub(crate) fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }

    fn accounts(&self) -> MutexGuard<'_, BTreeMap<String, Account>> {
        self.accounts.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl AccountDirectory for InMemoryAccounts {
    fn authenticate<'a>(
        &'a self,
        email: &'a str,
        password: &'a str,
    ) -> DirectoryFuture<'a, Authentication> {
        Box::pin(async move {
            if self.unavailable.load(Ordering::SeqCst) {
                return Err(DirectoryError::Unavailable {
                    detail: REFUSAL.to_owned(),
                });
            }

            let accounts = self.accounts();
            let Some(account) = accounts.get(email) else {
                return Ok(Authentication::Refused);
            };
            if account.locked {
                return Ok(Authentication::Locked);
            }
            if account.password == password {
                Ok(Authentication::Granted(account.user_id.clone()))
            } else {
                Ok(Authentication::Refused)
            }
        })
    }
}

// ===========================================================================================
// Session store double
// ===========================================================================================

/// `S-C29`'s in-memory session store, with a switch that makes it unreachable.
///
/// Delegation rather than reimplementation: when the switch is off this *is* the adapter the
/// shared conformance suite passes, so a test asserting session state is asserting against the
/// real thing. `ttl()` answers either way — a store that could not report its own configured
/// lifetime would be a different failure from a store that cannot be reached.
#[derive(Debug)]
pub(crate) struct SwitchableSessions {
    inner: InMemoryAuthState,
    unavailable: AtomicBool,
}

impl SwitchableSessions {
    /// A working store on `clock`, with [`SESSION_TTL`].
    pub(crate) fn new(clock: Arc<ManualClock>) -> Self {
        Self {
            inner: InMemoryAuthState::new(clock, SESSION_TTL),
            unavailable: AtomicBool::new(false),
        }
    }

    /// Make every subsequent operation fail, or stop.
    pub(crate) fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }

    fn refuse<T>() -> Result<T, StoreError> {
        Err(StoreError::Unavailable {
            store: "auth-state",
            detail: REFUSAL.to_owned(),
        })
    }

    fn is_down(&self) -> bool {
        self.unavailable.load(Ordering::SeqCst)
    }
}

impl AuthStateStore for SwitchableSessions {
    fn ttl(&self) -> SignedDuration {
        self.inner.ttl()
    }

    fn open_session(&self, record: SessionRecord) -> StoreFuture<'_, ()> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.open_session(record)
    }

    fn read_session<'a>(
        &'a self,
        session: &'a SessionId,
    ) -> StoreFuture<'a, Option<SessionRecord>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.read_session(session)
    }

    fn touch_session<'a>(
        &'a self,
        session: &'a SessionId,
        last_active_at: Timestamp,
    ) -> StoreFuture<'a, Option<SessionRecord>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.touch_session(session, last_active_at)
    }

    fn close_session<'a>(
        &'a self,
        session: &'a SessionId,
    ) -> StoreFuture<'a, Option<SessionRecord>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.close_session(session)
    }

    fn sessions_for_user<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Vec<SessionRecord>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.sessions_for_user(user)
    }

    fn close_all_for_user<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Vec<SessionRecord>> {
        if self.is_down() {
            return Box::pin(async { Self::refuse() });
        }
        self.inner.close_all_for_user(user)
    }
}

// ===========================================================================================
// The fixture
// ===========================================================================================

/// A built server, plus handles on everything behind it.
///
/// The handles matter: an assertion about a session is made against the store the server just
/// wrote to, not against a second reading of the response body.
pub(crate) struct Fixture {
    /// The in-process client. No socket, no port, no runtime flavour.
    pub(crate) client: TestClient<App>,
    /// The store the server opened its sessions in.
    pub(crate) sessions: Arc<SwitchableSessions>,
    /// The directory the server authenticated against.
    pub(crate) accounts: Arc<InMemoryAccounts>,
    /// The signer the server minted with — the *same* one, so a test can mint a token the
    /// server will accept, or one it must not.
    pub(crate) tokens: Arc<SessionTokens>,
    /// The clock every record and every deadline is stamped from.
    pub(crate) clock: Arc<ManualClock>,
}

impl Fixture {
    /// A server whose collaborators all work, with one account seeded.
    pub(crate) fn working() -> Self {
        let clock = Arc::new(ManualClock::default());
        let sessions = Arc::new(SwitchableSessions::new(clock.clone()));
        let accounts = Arc::new(InMemoryAccounts::new());
        accounts.insert(EMAIL, PASSWORD, &user());
        let tokens = Arc::new(signer(clock.clone()));

        let app = App::with_auth(
            sessions.clone(),
            accounts.clone(),
            tokens.clone(),
            clock.clone(),
        );

        Self {
            client: TestClient::new(capsule_server::service(app).expect("the router builds")),
            sessions,
            accounts,
            tokens,
            clock,
        }
    }

    /// Just the application context, for a test that needs no handles on what is behind it.
    pub(crate) fn working_app() -> App {
        let clock = Arc::new(ManualClock::default());
        let accounts = Arc::new(InMemoryAccounts::new());
        accounts.insert(EMAIL, PASSWORD, &user());
        App::with_auth(
            Arc::new(SwitchableSessions::new(clock.clone())),
            accounts,
            Arc::new(signer(clock.clone())),
            clock,
        )
    }

    /// Sign in with the seeded account and return the pair.
    pub(crate) async fn login(&self) -> capsule_server::routes::auth::TokenResponse {
        self.client
            .post("/v1/auth/login")
            .header("accept", "application/json")
            .json(&serde_json::json!({ "email": EMAIL, "password": PASSWORD }))
            .send()
            .await
            .assert_status(kynos::http::StatusCode::OK)
            .json()
    }
}

/// The account [`Fixture::working`] seeds.
pub(crate) fn user() -> UserId {
    UserId::new("01937b7c-0000-7000-8000-000000000001")
}

/// A signer over a freshly generated Ed25519 key pair.
///
/// Generated rather than read from a checked-in PEM: a private key in the repository is a
/// private key somebody eventually reuses.
pub(crate) fn signer(clock: Arc<ManualClock>) -> SessionTokens {
    use ring::signature::KeyPair as _;

    let der = ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
        .expect("the platform can generate an Ed25519 key");
    let pair = ring::signature::Ed25519KeyPair::from_pkcs8_maybe_unchecked(der.as_ref())
        .expect("a key just generated parses");

    SessionTokens::new(
        EncodingKey::from_ed_der(der.as_ref()),
        DecodingKey::from_ed_der(pair.public_key().as_ref()),
        clock,
    )
}
