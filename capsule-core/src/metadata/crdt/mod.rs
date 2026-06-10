//! CRDT semantics for collaborative metadata (SSoT: [Metadata — Collaborative Metadata]).
//!
//! User-editable fields on a shared album (tags, captions, ratings) can be edited
//! concurrently across devices, including offline. They are modelled as CRDTs so merges are
//! deterministic and commutative:
//!
//! - [`or_set::OrSet`] — tags, with `add_id` binding and reject-unobserved-remove.
//! - [`lww::Lww`] — single-value registers (caption, rating) with a superseded log.
//! - [`counter::Counter`] — the per-device monotonic `add_id` counter.
//!
//! [Metadata — Collaborative Metadata]: https://docs/design/metadata/#collaborative-metadata

pub mod counter;
pub mod lww;
pub mod or_set;

pub use counter::Counter;
pub use lww::{Lww, Stamped};
pub use or_set::{AddId, OrSet, UnobservedRemove};
