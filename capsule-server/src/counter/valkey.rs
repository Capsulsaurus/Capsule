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

use jiff::Timestamp;

use super::{Budget, CounterKey, CounterStore, Verdict};
use crate::store::valkey::{Valkey, from_micros, micros};
use crate::store::{StoreError, StoreFuture};

/// The port name, for the log line and the error.
const COUNTERS: &str = "counters";

/// One Lua script, built on first use. Private to this module; `store::valkey` has its own.
struct Lua {
    source: &'static str,
    script: std::sync::OnceLock<redis::Script>,
}

impl Lua {
    const fn new(source: &'static str) -> Self {
        Self {
            source,
            script: std::sync::OnceLock::new(),
        }
    }

    fn script(&self) -> &redis::Script {
        self.script.get_or_init(|| redis::Script::new(self.source))
    }
}

// KEYS: counter. ARGV: at (µs), window (µs), limit, window (ms, for the collector).
// Returns {hits after this call, or -1 when refused; the instant the window ends (µs)}.
static HIT: Lua = Lua::new(
    "local at = tonumber(ARGV[1])
local window = tonumber(ARGV[2])
local limit = tonumber(ARGV[3])
local opened = redis.call('HGET', KEYS[1], 'opened_at')
if not opened or tonumber(opened) + window <= at then
  redis.call('HSET', KEYS[1], 'hits', 1, 'opened_at', ARGV[1])
  redis.call('PEXPIRE', KEYS[1], ARGV[4])
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

/// Valkey [`CounterStore`].
#[derive(Debug)]
pub struct ValkeyCounters {
    valkey: Valkey,
}

impl ValkeyCounters {
    /// Counters on `valkey`.
    pub fn new(valkey: Valkey) -> Self {
        Self { valkey }
    }

    async fn run(
        &self,
        script: &Lua,
        key: &CounterKey,
        args: &[String],
    ) -> Result<(i64, i64), StoreError> {
        let mut invocation = script.script().prepare_invoke();
        invocation.key(counter_key(key));
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
            let (hits, ends) = self
                .run(
                    &HIT,
                    key,
                    &[
                        micros(at).to_string(),
                        window_micros,
                        budget.limit.to_string(),
                        window_millis,
                    ],
                )
                .await?;
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
            let mut cmd = redis::cmd("DEL");
            cmd.arg(counter_key(key));
            let removed: u64 = self.valkey.command(COUNTERS, cmd).await?;
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
