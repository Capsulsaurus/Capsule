//! Last-writer-wins register with a bounded *superseded* log (SSoT: [Metadata —
//! Surfacing Concurrent Edits]).
//!
//! Single-value collaborative fields (caption, rating) are LWW registers keyed by a signed
//! timestamp with the writing `device_id` as the lexicographic tiebreaker. A plain LWW
//! loses one side of a tied edit silently; Capsule instead keeps the winner authoritative
//! **and** preserves displaced values in a `superseded` log (capped, oldest evicted), so a
//! buggy client clobbering another device's edit becomes an explicit, recoverable surface.
//!
//! [Metadata — Surfacing Concurrent Edits]: https://docs/design/metadata/#surfacing-concurrent-edits

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Default cap on the superseded log (see Metadata: `superseded_captions ≤ 16`).
pub const SUPERSEDED_CAP: usize = 16;

/// A timestamped, device-attributed value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamped<T> {
    /// The value.
    pub value: T,
    /// RFC3339 write time.
    pub ts: String,
    /// Writing device id (the tiebreaker).
    pub by: Uuid,
}

/// An LWW register that also retains displaced values, newest-superseded first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lww<T: Clone + PartialEq> {
    /// The current authoritative value, if any.
    pub current: Option<Stamped<T>>,
    /// Displaced values (most recently superseded first), capped at [`SUPERSEDED_CAP`].
    pub superseded: Vec<Stamped<T>>,
}

/// Order two candidates: later timestamp wins; ties break on the larger device id.
/// `None` if a timestamp is unparseable (caller treats as a structural reject upstream).
fn beats(a: &Stamped<impl Clone>, b: &Stamped<impl Clone>) -> Option<bool> {
    let ta = chrono::DateTime::parse_from_rfc3339(&a.ts).ok()?;
    let tb = chrono::DateTime::parse_from_rfc3339(&b.ts).ok()?;
    Some((ta, a.by) > (tb, b.by))
}

impl<T: Clone + PartialEq> Lww<T> {
    /// An empty register.
    pub fn new() -> Self {
        Self {
            current: None,
            superseded: Vec::new(),
        }
    }

    /// Apply a write. The higher `(ts, device_id)` becomes current; the loser is recorded
    /// in `superseded` (capped). Returns `false` if a timestamp was unparseable (no change).
    pub fn set(&mut self, value: T, ts: impl Into<String>, by: Uuid) -> bool {
        let incoming = Stamped {
            value,
            ts: ts.into(),
            by,
        };
        match &self.current {
            None => {
                self.current = Some(incoming);
                true
            }
            Some(cur) => match beats(&incoming, cur) {
                None => false,
                Some(true) => {
                    let loser = self.current.replace(incoming).unwrap();
                    self.push_superseded(loser);
                    true
                }
                Some(false) => {
                    // Incoming loses (or equals) — keep it as a superseded alternative if
                    // it is genuinely a different value, not a duplicate of current.
                    if Some(&incoming.value) != self.current.as_ref().map(|c| &c.value) {
                        self.push_superseded(incoming);
                    }
                    true
                }
            },
        }
    }

    fn push_superseded(&mut self, s: Stamped<T>) {
        self.superseded.insert(0, s);
        self.superseded.truncate(SUPERSEDED_CAP);
    }

    /// Merge another replica's register: apply its current and all superseded entries.
    pub fn merge(&mut self, other: &Self) {
        if let Some(c) = &other.current {
            self.set(c.value.clone(), c.ts.clone(), c.by);
        }
        for s in &other.superseded {
            self.set(s.value.clone(), s.ts.clone(), s.by);
        }
    }

    /// The current value, if any.
    pub fn get(&self) -> Option<&T> {
        self.current.as_ref().map(|s| &s.value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    #[test]
    fn later_timestamp_wins_and_loser_is_superseded() {
        let mut r = Lww::new();
        r.set("first".to_string(), "2026-05-31T10:00:00Z", dev(1));
        r.set("second".to_string(), "2026-05-31T11:00:00Z", dev(2));
        assert_eq!(r.get(), Some(&"second".to_string()));
        assert_eq!(r.superseded.len(), 1);
        assert_eq!(r.superseded[0].value, "first");
    }

    #[test]
    fn tie_breaks_on_larger_device_id() {
        // Same timestamp, two devices → the larger device id wins; the other is superseded.
        let mut r = Lww::new();
        r.set("from-dev-1".to_string(), "2026-05-31T10:00:00Z", dev(1));
        r.set("from-dev-9".to_string(), "2026-05-31T10:00:00Z", dev(9));
        assert_eq!(r.get(), Some(&"from-dev-9".to_string()));
        assert_eq!(r.superseded[0].value, "from-dev-1");
    }

    #[test]
    fn an_earlier_write_arriving_late_does_not_clobber() {
        let mut r = Lww::new();
        r.set("new".to_string(), "2026-05-31T11:00:00Z", dev(2));
        // An older edit arrives after the newer one: current is unchanged, loser recorded.
        r.set("old".to_string(), "2026-05-31T09:00:00Z", dev(1));
        assert_eq!(r.get(), Some(&"new".to_string()));
        assert!(r.superseded.iter().any(|s| s.value == "old"));
    }

    #[test]
    fn superseded_log_is_capped() {
        let mut r = Lww::new();
        for i in 0..(SUPERSEDED_CAP + 5) {
            // Each later write wins and pushes the prior winner to superseded.
            let ts = format!("2026-05-31T{:02}:00:00Z", i);
            r.set(format!("v{i}"), ts, dev(1));
        }
        assert_eq!(r.superseded.len(), SUPERSEDED_CAP);
        // Most-recently superseded is first.
        assert_eq!(r.superseded[0].value, format!("v{}", SUPERSEDED_CAP + 3));
    }

    #[test]
    fn merge_converges() {
        let mut a = Lww::new();
        a.set("a".to_string(), "2026-05-31T10:00:00Z", dev(1));
        let mut b = Lww::new();
        b.set("b".to_string(), "2026-05-31T11:00:00Z", dev(2));

        let mut ab = a.clone();
        ab.merge(&b);
        let mut ba = b.clone();
        ba.merge(&a);
        assert_eq!(ab.get(), ba.get());
        assert_eq!(ab.get(), Some(&"b".to_string()));
    }

    #[test]
    fn unparseable_timestamp_is_a_no_op() {
        let mut r = Lww::new();
        r.set("ok".to_string(), "2026-05-31T10:00:00Z", dev(1));
        assert!(!r.set("bad".to_string(), "not-a-date", dev(2)));
        assert_eq!(r.get(), Some(&"ok".to_string()));
    }
}
