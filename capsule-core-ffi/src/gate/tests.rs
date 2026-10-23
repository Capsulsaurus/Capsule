use super::*;

#[test]
fn gated_view_round_trips_through_the_core_type() {
    for v in [GatedView::RecentlyDeleted, GatedView::Hidden] {
        assert_eq!(GatedView::from(core_gate::GatedView::from(v)), v);
    }
}

#[test]
fn local_auth_error_round_trips_through_the_core_type() {
    for e in [
        LocalAuthError::Cancelled,
        LocalAuthError::Unavailable,
        LocalAuthError::Failed,
    ] {
        assert_eq!(
            LocalAuthError::from(core_gate::LocalAuthError::from(e.clone())),
            e
        );
    }
}
