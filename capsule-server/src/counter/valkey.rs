//! The Valkey [`CounterStore`] (#403).
//!
//! One hash per key — `capsule:counter:{kind}:{scope}` holding `hits` and `opened_at` — and one
//! Lua script that opens the window, or charges it, or refuses, in a single server-side step.
//! That is the atomicity the port demands: a burst of requests cannot read the same under-limit
//! count, because there is no read a caller performs separately from the charge.
//!
//! # Why a hash and a script rather than `INCR` + first-hit `EXPIRE`
//!
//! The port's window is measured from the caller's `at`, which is what lets the in-memory double
//! be deterministic and the [`conformance`](super::conformance) suite pass absolute instants. A
//! window that `EXPIRE` measured on the server's clock instead would be a second clock for one
//! fact, and the same suite could not drive both adapters. So `opened_at` is stored and compared
//! against `at` inside the script; `PEXPIRE` is set to the window as well, but only so a key
//! nobody hits again is collected — it never decides anything.
//!
//! An over-budget hit is refused **without** being counted, exactly as the double does it: the
//! window's end is fixed by its first hit, so a refused hit cannot extend it.
//!
//! # The ceiling is per [`CounterKey`] variant, here as in the double (#519)
//!
//! The port's contract — see [the module docs](super) — is that each variant is a partition of
//! its own, bounded by [`CounterKey::ceiling`], and that past its ceiling a key with no open
//! window is refused with [`StoreError::Rejected`] under [`COUNTER_STORE`] while every key that
//! already has one keeps counting. `PEXPIRE` alone bounds how *long* a key lives, not how *many*
//! there are, so without the ceiling a caller-controlled key space is Valkey memory an
//! unauthenticated caller fills a key at a time.
//!
//! So each variant has one sorted set, `capsule:counter_index:{kind}`, whose members are the
//! scopes holding an open window, scored by the instant that window ends (µs). The `HIT` script
//! consults it only when it is about to **open** a window: it first drops every member whose
//! window has ended (`ZREMRANGEBYSCORE … at`, the double's purge-on-write), then refuses if the
//! partition is at its ceiling, and otherwise opens the window and indexes it — all in the one
//! atomic step, so a burst of first hits cannot all read the same under-ceiling count. A hit on a
//! window already open never touches the index, which is what keeps an admitted key counting
//! however full its partition becomes. The index's own `PEXPIRE` is raised to the longest window
//! it has seen and never lowered, so a partition nobody hits again is collected like the windows
//! it indexes.

use jiff::Timestamp;

use super::{Budget, COUNTER_STORE, CounterKey, CounterStore, Verdict};
use crate::store::valkey::{Lua, Valkey, from_micros, micros};
use crate::store::{StoreError, StoreFuture};

/// The port name, for the log line and the error — the name [`super::is_at_capacity`] matches.
const COUNTERS: &str = COUNTER_STORE;

/// What `HIT` answers in place of a hit count when the partition is full.
const AT_CEILING: i64 = -2;

// KEYS: counter, its partition's index. ARGV: at (µs), window (µs), limit, window (ms, for the
// collector), ceiling, scope (the index member).
// Returns {hits after this call, or -1 when limited, or -2 when the partition is full; the
// instant the window ends (µs), or the partition's occupancy when full}.
static HIT: Lua = Lua::new(
    "local at = tonumber(ARGV[1])
local window = tonumber(ARGV[2])
local limit = tonumber(ARGV[3])
local opened = redis.call('HGET', KEYS[1], 'opened_at')
if not opened or tonumber(opened) + window <= at then
  redis.call('ZREMRANGEBYSCORE', KEYS[2], '-inf', ARGV[1])
  local occupancy = redis.call('ZCARD', KEYS[2])
  if occupancy >= tonumber(ARGV[5]) and not redis.call('ZSCORE', KEYS[2], ARGV[6]) then
    return {-2, occupancy}
  end
  redis.call('HSET', KEYS[1], 'hits', 1, 'opened_at', ARGV[1])
  redis.call('PEXPIRE', KEYS[1], ARGV[4])
  redis.call('ZADD', KEYS[2], at + window, ARGV[6])
  if redis.call('PTTL', KEYS[2]) < tonumber(ARGV[4]) then
    redis.call('PEXPIRE', KEYS[2], ARGV[4])
  end
  return {1, at + window}
end
local ends = tonumber(opened) + window
local hits = tonumber(redis.call('HGET', KEYS[1], 'hits') or 0)
if hits >= limit then return {-1, ends} end
hits = redis.call('HINCRBY', KEYS[1], 'hits', 1)
return {hits, ends}",
);

// KEYS: counter. ARGV: at (µs), window (µs). Returns {hits in the open window, or 0; its end}.
static PEEK: Lua = Lua::new(
    "local at = tonumber(ARGV[1])
local window = tonumber(ARGV[2])
local opened = redis.call('HGET', KEYS[1], 'opened_at')
if not opened or tonumber(opened) + window <= at then return {0, 0} end
return {tonumber(redis.call('HGET', KEYS[1], 'hits') or 0), tonumber(opened) + window}",
);

// KEYS: counter, its partition's index. ARGV: scope. Returns how many of the two were removed.
// One step, so a reset can never leave a window counted against the ceiling with no window
// behind it, nor a window with no place in its partition.
static RESET: Lua =
    Lua::new("return redis.call('DEL', KEYS[1]) + redis.call('ZREM', KEYS[2], ARGV[1])");

/// Valkey [`CounterStore`].
#[derive(Debug)]
pub struct ValkeyCounters {
    valkey: Valkey,
    /// Set by [`ValkeyCounters::with_ceiling`], and then the ceiling of **every** partition.
    /// `None` in production, where each variant carries its own [`CounterKey::ceiling`].
    ceiling_override: Option<usize>,
}

impl ValkeyCounters {
    /// Counters on `valkey`, each variant bounded by its own [`CounterKey::ceiling`].
    pub fn new(valkey: Valkey) -> Self {
        Self {
            valkey,
            ceiling_override: None,
        }
    }

    /// The same counters with **every** partition bounded at `ceiling` instead.
    ///
    /// The affordance [`InMemoryCounters::with_ceiling`](super::InMemoryCounters::with_ceiling)
    /// is, for the same reason: it makes the partition boundary observable against a real server
    /// without writing twenty thousand keys. Production leaves it unset.
    #[must_use]
    pub fn with_ceiling(mut self, ceiling: usize) -> Self {
        self.ceiling_override = Some(ceiling);
        self
    }

    /// This key's partition ceiling, or the override every partition shares when one is set.
    fn ceiling_for(&self, key: &CounterKey) -> usize {
        self.ceiling_override.unwrap_or_else(|| key.ceiling())
    }

    async fn run(
        &self,
        script: &Lua,
        key: &CounterKey,
        args: &[String],
    ) -> Result<(i64, i64), StoreError> {
        let mut invocation = script.script().prepare_invoke();
        invocation.key(counter_key(key)).key(index_key(key));
        for arg in args {
            invocation.arg(arg.as_str());
        }
        self.valkey.invoke(COUNTERS, &invocation).await
    }
}

/// `capsule:counter:{kind}:{scope}`.
fn counter_key(key: &CounterKey) -> String {
    format!("capsule:counter:{}:{}", key.as_str(), key.scope())
}

/// `capsule:counter_index:{kind}` — the partition [`CounterKey::ceiling`] bounds.
///
/// A prefix of its own rather than a segment under `capsule:counter:`, so no `{kind}:{scope}`
/// pair can ever spell an index key.
fn index_key(key: &CounterKey) -> String {
    format!("capsule:counter_index:{}", key.as_str())
}

/// The refusal a full partition answers, in the shape the double answers it.
fn at_ceiling(key: &CounterKey, ceiling: usize, occupancy: i64) -> StoreError {
    tracing::warn!(
        counter = key.as_str(),
        windows = occupancy,
        ceiling,
        "a counter partition is full; a hit was refused rather than counted"
    );
    StoreError::Rejected {
        store: COUNTER_STORE,
        detail: format!(
            "{ceiling} open windows is the ceiling for `{}`",
            key.as_str()
        ),
    }
}

/// A window as the microsecond string the scripts compare, and the millisecond string
/// `PEXPIRE` takes.
fn window_args(budget: Budget) -> (String, String) {
    let micros = i64::try_from(budget.window.as_micros()).unwrap_or(i64::MAX);
    let millis = budget.window.as_millis().max(1);
    (micros.to_string(), millis.to_string())
}

fn admitted(limit: u32, hits: i64) -> Verdict {
    Verdict::Admitted {
        remaining: limit.saturating_sub(u32::try_from(hits).unwrap_or(u32::MAX)),
    }
}

impl CounterStore for ValkeyCounters {
    fn hit<'a>(
        &'a self,
        key: &'a CounterKey,
        budget: Budget,
        at: Timestamp,
    ) -> StoreFuture<'a, Verdict> {
        Box::pin(async move {
            let (window_micros, window_millis) = window_args(budget);
            let ceiling = self.ceiling_for(key);
            let (hits, ends) = self
                .run(
                    &HIT,
                    key,
                    &[
                        micros(at).to_string(),
                        window_micros,
                        budget.limit.to_string(),
                        window_millis,
                        ceiling.to_string(),
                        key.scope().to_owned(),
                    ],
                )
                .await?;
            if hits == AT_CEILING {
                return Err(at_ceiling(key, ceiling, ends));
            }
            if hits < 0 {
                let retry_after = from_micros(COUNTERS, "Window", ends)?;
                tracing::info!(counter = key.as_str(), %retry_after, "a rate limit engaged");
                return Ok(Verdict::Limited { retry_after });
            }
            tracing::trace!(counter = key.as_str(), hits, "charged a counter");
            Ok(admitted(budget.limit, hits))
        })
    }

    fn peek<'a>(
        &'a self,
        key: &'a CounterKey,
        budget: Budget,
        at: Timestamp,
    ) -> StoreFuture<'a, Verdict> {
        Box::pin(async move {
            let (window_micros, _) = window_args(budget);
            let (hits, ends) = self
                .run(&PEEK, key, &[micros(at).to_string(), window_micros])
                .await?;
            // No open window is a fresh budget, whatever the limit — including zero, which the
            // double answers `Admitted { remaining: 0 }` rather than `Limited`.
            if ends == 0 {
                return Ok(admitted(budget.limit, 0));
            }
            if hits >= i64::from(budget.limit) {
                return Ok(Verdict::Limited {
                    retry_after: from_micros(COUNTERS, "Window", ends)?,
                });
            }
            Ok(admitted(budget.limit, hits))
        })
    }

    fn reset<'a>(&'a self, key: &'a CounterKey) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            let mut invocation = RESET.script().prepare_invoke();
            invocation
                .key(counter_key(key))
                .key(index_key(key))
                .arg(key.scope());
            let removed: u64 = self.valkey.invoke(COUNTERS, &invocation).await?;
            if removed > 0 {
                tracing::debug!(counter = key.as_str(), "a counter window was cleared");
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::UserId;

    #[test]
    fn a_counter_key_names_its_kind_and_its_scope() {
        let key = CounterKey::ShareSource("203.0.113.7".to_owned());
        assert_eq!(
            counter_key(&key),
            "capsule:counter:share_source:203.0.113.7"
        );
        let key = CounterKey::LoginAttempts(UserId::new("u1"));
        assert_eq!(counter_key(&key), "capsule:counter:login_attempts:u1");
    }

    #[test]
    fn a_partition_index_names_its_kind_and_no_scope() {
        let key = CounterKey::DropLink("d1".to_owned());
        assert_eq!(index_key(&key), "capsule:counter_index:drop_link");
        assert_eq!(
            index_key(&key),
            index_key(&CounterKey::DropLink("d2".to_owned())),
            "every scope of one variant shares one partition"
        );
        assert_ne!(
            index_key(&key),
            index_key(&CounterKey::ShareLink("d1".to_owned())),
            "and no two variants share one"
        );
    }

    #[test]
    fn a_full_partition_is_the_refusal_is_at_capacity_recognises() {
        let key = CounterKey::DropLink("d1".to_owned());
        let error = at_ceiling(&key, 2, 2);
        assert!(super::super::is_at_capacity(&error), "{error:?}");
        assert!(error.to_string().contains("drop_link"), "{error}");
    }

    #[test]
    fn a_window_is_passed_in_both_units() {
        let (micros, millis) = window_args(Budget::new(3, jiff::SignedDuration::from_secs(2)));
        assert_eq!(micros, "2000000");
        assert_eq!(millis, "2000");
    }

    #[test]
    fn remaining_never_underflows() {
        assert_eq!(admitted(3, 5), Verdict::Admitted { remaining: 0 });
        assert_eq!(admitted(3, 1), Verdict::Admitted { remaining: 2 });
    }
}
