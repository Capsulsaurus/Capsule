//! The budgets this deployment enforces, in one place.
//!
//! Every limit the server applies is declared here rather than at its call site. That is not
//! tidiness: a budget written inline is a budget nobody can review against the threat model, and
//! two surfaces that ought to share a limit — the share path's two limiters and the drop path's
//! two, which the contracts explicitly call *the same two limiters* — would drift apart the
//! first time one was tuned.
//!
//! The numbers are deliberately conservative and deliberately adjustable. What is **not**
//! adjustable is where they live.

use jiff::SignedDuration;

use super::Budget;

/// Failed sign-ins per account before the account is locked out for a window.
///
/// Five in fifteen minutes. Low, because the thing being guessed is a password and the cost of a
/// legitimate user waiting is a support conversation while the cost of getting it wrong is an
/// account.
pub const LOGIN_ATTEMPTS: Budget = Budget::new(5, SignedDuration::from_mins(15));

/// Redemption attempts presenting one spelling of a code, deployment-wide (issue #528).
///
/// Thirty a minute against each
/// [`CounterKey::EnrollmentRedemption`](crate::counter::CounterKey::EnrollmentRedemption)
/// bucket. This is the limiter design/device-enrollment.md names as the reason the transcribable
/// fallback is safe to offer at all, and the number is that argument:
///
/// - A wrong guess names no pending enrollment, so it can only be counted against the space it
///   searches. Every fallback guess, from every address, spends this one budget.
/// - A pending enrollment lives ten minutes, which is at most eleven of these fixed one-minute
///   windows, so at most 330 fallback guesses are admitted across its whole life.
/// - Each guess matches at most one of the 10^8 eight-digit fallbacks. So the chance that any
///   one pending enrollment is guessed inside its lifetime is at most 330 in 10^8 — about three
///   in a million, the same order as [`SECOND_FACTOR`]'s five in a million per sign-in.
///
/// Deliberately blunt in the other direction: a flood spends the fallback bucket for everyone,
/// for a minute at a time. That is the trade, and it is a bounded one — the QR spelling has its
/// own bucket, so a person whose typed code is throttled can still scan — where the per-code key
/// this replaced bounded nothing, because every wrong guess was a fresh string with a fresh
/// budget. Thirty a minute is far above a deployment's honest rate of device adds.
///
/// The full-entropy spelling is charged the same budget in its own bucket. Its 122 random bits
/// already defeat guessing; the budget there exists so that presenting one is not free.
pub const ENROLLMENT_REDEMPTION: Budget = Budget::new(30, SignedDuration::from_mins(1));

/// Redemption attempts presenting something that is not shaped like a code at all.
///
/// Sixty a minute against
/// [`CounterKey::EnrollmentRedemptionMalformed`](crate::counter::CounterKey::EnrollmentRedemptionMalformed)'s
/// one bucket, the same shape the refused-redirect bucket takes. Deliberately more generous
/// than [`ENROLLMENT_REDEMPTION`]: a malformed code is not a guess at any pending enrollment —
/// it cannot match one — so this number is not part of the entropy argument that budget
/// carries. It exists so that a malformed attempt is not free, and so the
/// partition holding it can never be more than one key wide.
pub const ENROLLMENT_REDEMPTION_MALFORMED: Budget = Budget::new(60, SignedDuration::from_mins(1));

/// Requests against one share link's opaque id.
///
/// Sixty a minute: generous for a person opening a shared album, and a hard ceiling on how fast
/// one link can be probed. Enumeration across *many* ids is bounded by
/// [`SHARE_SOURCE`] and, structurally, by the 128-bit id itself.
pub const SHARE_LINK: Budget = Budget::new(60, SignedDuration::from_mins(1));

/// Requests from one source address on the public share path.
///
/// A hundred and twenty a minute. Higher than the per-link budget because one household behind
/// one address legitimately opens several shares; low enough that walking the id space from one
/// address is hopeless long before the entropy is.
pub const SHARE_SOURCE: Budget = Budget::new(120, SignedDuration::from_mins(1));

/// Drop-session creations against one upload link (invariant 31).
///
/// Thirty an hour. A guest depositing a holiday's photos makes tens of requests; a script
/// filling somebody's quota makes thousands.
pub const DROP_LINK: Budget = Budget::new(30, SignedDuration::from_hours(1));

/// Drop-session creations from one source address (invariant 31, #704).
///
/// Sixty an hour, twice the per-link budget: one guest behind one address legitimately deposits
/// through a couple of links, while a script spraying fabricated ids from one address is held to
/// a minute's worth of honest use per hour. Keyed on
/// [`ClientAddr::source_key`](crate::client_addr::ClientAddr::source_key), so behind a proxy the
/// deployment must name it in `TRUSTED_PROXIES` or `TRUSTED_PROXY_HOPS`, or every guest shares
/// the proxy's budget.
pub const DROP_SOURCE: Budget = Budget::new(60, SignedDuration::from_hours(1));

/// Code attempts against one second-factor challenge.
///
/// Five in five minutes, which is the challenge's whole lifetime — so the budget is really
/// "five attempts per sign-in". Six digits is a million codes and five guesses is a one-in-two-
/// hundred-thousand chance per ceremony, while a person mistyping twice and getting it right on
/// the third go is unaffected. The retired surface allowed three; five is chosen over three
/// because a code that expires mid-typing costs an attempt through no fault of the user.
pub const SECOND_FACTOR: Budget = Budget::new(5, SignedDuration::from_mins(5));

/// Begun OIDC ceremonies per **admitted** redirect host (`S-N1`).
///
/// Sixty a minute. A person signing in begins one; a browser that retries a few times begins a
/// handful; a script filling the pending-ceremony store begins thousands. The key space is
/// three hosts at most (the configured redirect and the two loopback literals) *because the
/// route validates the redirect before it charges this budget*, so this is close to a
/// deployment-wide ceiling: at the ten-minute ceremony TTL it caps the in-memory store at
/// well under two thousand live records against its ten-thousand ceiling.
pub const OIDC_AUTHORIZE: Budget = Budget::new(60, SignedDuration::from_mins(1));

/// OIDC authorizes whose redirect the policy refused, deployment-wide (`S-N1`).
///
/// Sixty a minute, the same number as the admitted path, against
/// [`CounterKey::OidcAuthorizeRefused`](crate::counter::CounterKey::OidcAuthorizeRefused)'s one
/// bucket. A refused authorize does no work worth throttling for its own sake — the redirect
/// check is a string comparison and nothing is written — so this budget is not protecting the
/// server's CPU. It is here so that "refused" is not the one request on the surface that costs
/// an attacker nothing to repeat, and so the log line that reports the refusal is itself
/// bounded.
pub const OIDC_AUTHORIZE_REFUSED: Budget = Budget::new(60, SignedDuration::from_mins(1));

/// Begun OIDC device authorizations, deployment-wide (issue #635).
///
/// Sixty a minute, the authorize's number, against
/// [`CounterKey::OidcDevice`](crate::counter::CounterKey::OidcDevice)'s one bucket: a person
/// signing in from a terminal begins one, and a script filling the device store begins
/// thousands. At the fifteen-minute device lifetime it holds the in-memory store under a thousand
/// live records against its ten-thousand ceiling, and every begin it refuses is one the provider
/// is never asked for.
pub const OIDC_DEVICE: Budget = Budget::new(60, SignedDuration::from_mins(1));

/// Deep storage verifications per account.
///
/// Four an hour. The contract calls the limiter *half of the feature*: a deep verify reads and
/// re-hashes every declared blob, so an unbounded one is an I/O-amplification attack costing the
/// attacker one small JSON body.
pub const DEEP_VERIFY: Budget = Budget::new(4, SignedDuration::from_hours(1));

/// Requests from one federated peer, across the sync and blob reads (invariant 21).
///
/// Ten thousand an hour — the retired server's established-tier default. A peer pulling a
/// shared album makes one sync page and a few blob fetches per asset; ten thousand is an
/// evening of photos from one household of peers, and a hostile peer enumerating addresses
/// gets fewer than three a second. A fixed window, so a peer that spends it waits for the
/// hour to turn rather than trickling back in.
pub const PEER_REQUESTS: Budget = Budget::new(10_000, SignedDuration::from_hours(1));

/// Federated moderation reports from one peer against one account (invariant 24).
///
/// Twenty an hour per `(reporting_server, reported_user)`. A real report is one message; a
/// flood against one user is the false-flag vector the contract names, and backpressure at
/// twenty bounds it without silencing a peer that has two things to say.
pub const FEDERATED_REPORTS: Budget = Budget::new(20, SignedDuration::from_hours(1));

/// Federated moderation reports from one peer against **every** account (`S-C49`).
///
/// Two hundred an hour. Ten times the per-account allowance, so a peer with a genuinely bad hour
/// — a spam wave it is reporting honestly — is not silenced, while a peer cycling `reported_user`
/// to mint itself a fresh per-account budget each time runs into a ceiling that does not care
/// which account it named.
pub const PEER_REPORTS: Budget = Budget::new(200, SignedDuration::from_hours(1));

/// Attempts at `POST /v1/federation/reports` from one **claimed** origin (`S-C49`).
///
/// Three hundred an hour, charged before the peer is looked up. Deliberately above
/// [`PEER_REPORTS`], because it is not a policy on reporting — it is the bound on how much work
/// an anonymous caller can ask for on the server's one unauthenticated write, and a real peer
/// must never meet it before meeting the budget that *is* the policy.
pub const FEDERATED_INTAKE: Budget = Budget::new(300, SignedDuration::from_hours(1));
