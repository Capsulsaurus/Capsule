//! Default-album resolution — where an import lands when the user files it nowhere (SSoT:
//! [Organization — The Default Album]).
//!
//! Capsule guarantees every owner a **default album**: a de facto, nameless container whose
//! id is derived from the account master key, so a device can locate it from the master key
//! alone. Which container is *currently* the default may be re-pointed by a non-secret
//! `default_album_id` on the owner record, and an explicit pick at import time overrides
//! both.
//!
//! [`resolve_default_album`] is the **one** place that order is encoded, and it records
//! which rule fired ([`ResolutionRule`]) so a surprising destination is explainable after
//! the fact. It **always** resolves to a container — a view can never be an import
//! destination.
//!
//! **v1 scope.** The doc's full order interposes two settings-document rows —
//! `scope_id` override, then per-[`SourceKind`](super::scope::SourceKind) default — between
//! the explicit pick and the owner pointer. Those rows ship post-v1 with the
//! library-settings document; v1 ships the base order below. The import's
//! [`Scope`] is carried through and recorded now, so the two lookups slot in
//! at the marked seam without changing this signature or any caller.
//!
//! [Organization — The Default Album]: https://docs/design/organization/#the-default-album

use uuid::Uuid;

use crate::crypto::hash::Hash32;
use crate::import::scope::Scope;

/// Which rule of the resolution order chose the destination album. Recorded on every
/// resolution so a surprising destination is explainable after the fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResolutionRule {
    /// Rule 1 — the user picked an album explicitly at import time.
    ExplicitPick,
    /// Rule 2 — the owner record's `default_album_id` pointer. (The post-v1 `scope_id`
    /// override and per-source-kind default rows interpose *before* this one.)
    OwnerPointer,
    /// Rule 3 — the de facto album derived from the account master key. The terminal rule:
    /// it exists for every owner from first-device enrollment, so import always has a home.
    DerivedDeFacto,
}

impl ResolutionRule {
    /// A stable, structured label for logs and diagnostics.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ResolutionRule::ExplicitPick => "explicit_pick",
            ResolutionRule::OwnerPointer => "owner_pointer",
            ResolutionRule::DerivedDeFacto => "derived_de_facto",
        }
    }
}

/// Everything the resolution order reads. Each field is one rule's input, in priority
/// order; [`scope`](DefaultAlbumContext::scope) is carried for traceability and for the
/// post-v1 override rows.
///
/// A context whose three album fields are all `None` is **unbound** — no library has been
/// attached to it yet. That is a caller-side gap, not a user-facing state: an owner always
/// has a derived de facto album, so binding it (see
/// [`with_derived`](DefaultAlbumContext::with_derived)) makes resolution total.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DefaultAlbumContext {
    /// Rule 1 — the album the user picked explicitly for this import, if any.
    pub explicit_pick: Option<Uuid>,
    /// Rule 2 — the owner record's `default_album_id` pointer, if known to this device.
    pub owner_default_album_id: Option<Uuid>,
    /// Rule 3 — the de facto album derived from the account master key. Only the library
    /// knows it, so callers bind it with [`with_derived`](Self::with_derived).
    pub derived_de_facto_album_id: Option<Uuid>,
    /// The import source this run came from. Not read by the v1 order; recorded on the
    /// resolution, and the seam the post-v1 override rows key off.
    pub scope: Option<Scope>,
}

impl DefaultAlbumContext {
    /// A context bound to a library's derived de facto album (rule 3) and nothing else —
    /// the "user filed it nowhere, no owner pointer known" baseline.
    #[must_use]
    pub fn derived(derived_de_facto_album_id: Uuid) -> Self {
        Self {
            derived_de_facto_album_id: Some(derived_de_facto_album_id),
            ..Self::default()
        }
    }

    /// This context with rule 3 bound to `derived` **if it is not already set**.
    ///
    /// The library is the only authority on the derived de facto album, so a caller that
    /// assembled the context without one (an [`ImportConfig`](super::ImportConfig) built
    /// before a workspace was opened) completes it here. An already-bound context is
    /// returned unchanged, so binding twice cannot change the answer.
    #[must_use]
    pub fn with_derived(&self, derived: Uuid) -> Self {
        let mut bound = self.clone();
        bound.derived_de_facto_album_id.get_or_insert(derived);
        bound
    }
}

/// The album an import will land in, and the rule that chose it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAlbum {
    /// The destination container album. Always a container — never a view.
    pub album_id: Uuid,
    /// Which rule of the order fired.
    pub rule: ResolutionRule,
    /// The [`scope_id`](Scope::scope_id) of the import source, when the context named a
    /// scope. Recorded for explainability; the post-v1 override rows are keyed by it.
    pub scope_id: Option<Hash32>,
}

/// Failure to resolve a destination album.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DefaultAlbumError {
    /// The context named no album at any rule — it was never bound to a library. Every
    /// owner has a derived de facto album from first-device enrollment onward, so this is a
    /// caller that has not supplied it, never a legitimate "this user has nowhere to
    /// import" state.
    #[error(
        "unbound default-album context: no explicit pick, no owner `default_album_id` \
         pointer, and no derived de facto album"
    )]
    Unbound,
}

/// Resolve the destination album for an import, first match wins (SSoT:
/// [Organization — The Default Album]).
///
/// 1. the explicit user pick at import time,
/// 2. *(post-v1: the `scope_id` override row, then the per-source-kind default row)*,
/// 3. the owner's `default_album_id` pointer,
/// 4. the derived de facto album.
///
/// Deterministic by construction — a pure function of the context, with no clock, no I/O
/// and no iteration order — and the fired [`ResolutionRule`] travels with the answer.
///
/// [Organization — The Default Album]: https://docs/design/organization/#the-default-album
pub fn resolve_default_album(
    context: &DefaultAlbumContext,
) -> Result<ResolvedAlbum, DefaultAlbumError> {
    let scope_id = context.scope.as_ref().map(Scope::scope_id);

    // Rule 1 — explicit user pick.
    // POST-V1 SEAM: the `scope_id` override row, then the per-`SourceKind` default row,
    // interpose here once the library-settings document ships. Both key off `scope_id` /
    // `context.scope`, which is why the scope is carried on the context today.
    let (album_id, rule) = if let Some(pick) = context.explicit_pick {
        (pick, ResolutionRule::ExplicitPick)
    } else if let Some(pointer) = context.owner_default_album_id {
        // Rule 2 — the owner record's pointer.
        (pointer, ResolutionRule::OwnerPointer)
    } else if let Some(derived) = context.derived_de_facto_album_id {
        // Rule 3 — the terminal rule; import always has a home.
        (derived, ResolutionRule::DerivedDeFacto)
    } else {
        tracing::warn!("default-album resolution refused: context is not bound to a library");
        return Err(DefaultAlbumError::Unbound);
    };

    tracing::debug!(
        %album_id,
        rule = rule.as_str(),
        scope = ?context.scope,
        "resolved import destination album"
    );
    Ok(ResolvedAlbum {
        album_id,
        rule,
        scope_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cohort::PlatformTag;
    use crate::import::scope::SourceKind;

    const PICK: Uuid = Uuid::from_u128(0x11);
    const POINTER: Uuid = Uuid::from_u128(0x22);
    const DERIVED: Uuid = Uuid::from_u128(0x33);

    fn full() -> DefaultAlbumContext {
        DefaultAlbumContext {
            explicit_pick: Some(PICK),
            owner_default_album_id: Some(POINTER),
            derived_de_facto_album_id: Some(DERIVED),
            scope: None,
        }
    }

    /// Resolution-order bullet 1: an explicit user pick wins over everything below it.
    #[test]
    fn explicit_pick_wins() {
        let r = resolve_default_album(&full()).unwrap();
        assert_eq!(r.album_id, PICK);
        assert_eq!(r.rule, ResolutionRule::ExplicitPick);
    }

    /// Resolution-order bullet 2 (v1's second rule): with no explicit pick, the owner's
    /// `default_album_id` pointer wins over the derived de facto album.
    #[test]
    fn owner_pointer_wins_when_no_explicit_pick() {
        let mut ctx = full();
        ctx.explicit_pick = None;
        let r = resolve_default_album(&ctx).unwrap();
        assert_eq!(r.album_id, POINTER);
        assert_eq!(r.rule, ResolutionRule::OwnerPointer);
    }

    /// Resolution-order bullet 3: the terminal rule. Import always has a home.
    #[test]
    fn derived_de_facto_is_the_terminal_rule() {
        let r = resolve_default_album(&DefaultAlbumContext::derived(DERIVED)).unwrap();
        assert_eq!(r.album_id, DERIVED);
        assert_eq!(r.rule, ResolutionRule::DerivedDeFacto);
    }

    /// A context never bound to a library resolves to nothing — a caller-side gap, refused
    /// rather than silently inventing a destination.
    #[test]
    fn unbound_context_is_refused() {
        assert_eq!(
            resolve_default_album(&DefaultAlbumContext::default()),
            Err(DefaultAlbumError::Unbound)
        );
    }

    /// Binding rule 3 completes an unbound context and never overrides one already bound —
    /// so binding is idempotent and cannot change an answer.
    #[test]
    fn with_derived_binds_once_and_is_idempotent() {
        let bound = DefaultAlbumContext::default().with_derived(DERIVED);
        assert_eq!(
            resolve_default_album(&bound).unwrap().album_id,
            DERIVED,
            "binding completes an unbound context"
        );
        assert_eq!(
            bound.with_derived(Uuid::from_u128(0x99)),
            bound,
            "re-binding must not move an already-bound album"
        );

        // A context that already resolves by a higher rule is untouched by binding.
        let picked = DefaultAlbumContext {
            explicit_pick: Some(PICK),
            ..DefaultAlbumContext::default()
        }
        .with_derived(DERIVED);
        assert_eq!(resolve_default_album(&picked).unwrap().album_id, PICK);
    }

    /// The resolution is a pure function of the context: same context, same answer, every
    /// time — the determinism the planner's explainability record depends on.
    #[test]
    fn resolution_is_deterministic() {
        let ctx = DefaultAlbumContext {
            explicit_pick: None,
            owner_default_album_id: Some(POINTER),
            derived_de_facto_album_id: Some(DERIVED),
            scope: Some(Scope::new(
                PlatformTag::Android,
                SourceKind::Screenshots,
                "Pictures/Screenshots",
            )),
        };
        let first = resolve_default_album(&ctx).unwrap();
        for _ in 0..8 {
            assert_eq!(resolve_default_album(&ctx).unwrap(), first);
        }
    }

    /// The scope travels with the answer so a destination is explainable after the fact,
    /// and is the seam the post-v1 override rows key off.
    #[test]
    fn scope_id_is_recorded_on_the_resolution() {
        let scope = Scope::new(
            PlatformTag::Ios,
            SourceKind::CameraRoll,
            "smartAlbumUserLibrary",
        );
        let ctx = DefaultAlbumContext {
            scope: Some(scope.clone()),
            ..DefaultAlbumContext::derived(DERIVED)
        };
        let r = resolve_default_album(&ctx).unwrap();
        assert_eq!(r.scope_id, Some(scope.scope_id()));

        // No scope named → nothing recorded, and the destination is unaffected.
        let r = resolve_default_album(&DefaultAlbumContext::derived(DERIVED)).unwrap();
        assert_eq!(r.scope_id, None);
        assert_eq!(r.album_id, DERIVED);
    }

    /// The scope is *not* an input to the v1 order: two different scopes resolve to the
    /// same album until the post-v1 override rows land.
    #[test]
    fn scope_does_not_change_the_v1_destination() {
        let a = DefaultAlbumContext {
            scope: Some(Scope::new(PlatformTag::Linux, SourceKind::Folder, "a")),
            ..DefaultAlbumContext::derived(DERIVED)
        };
        let b = DefaultAlbumContext {
            scope: Some(Scope::new(PlatformTag::Linux, SourceKind::WatchedDir, "b")),
            ..DefaultAlbumContext::derived(DERIVED)
        };
        let (ra, rb) = (
            resolve_default_album(&a).unwrap(),
            resolve_default_album(&b).unwrap(),
        );
        assert_eq!(ra.album_id, rb.album_id);
        assert_eq!(ra.rule, rb.rule);
        assert_ne!(ra.scope_id, rb.scope_id);
    }
}
