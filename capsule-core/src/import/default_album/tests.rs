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
