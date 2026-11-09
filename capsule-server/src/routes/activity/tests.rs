use super::*;

#[test]
fn no_limit_is_the_default_page() {
    assert_eq!(page_size(None), DEFAULT_LIMIT);
}

#[test]
fn a_limit_inside_the_bound_is_kept() {
    assert_eq!(page_size(Some(0)), 0);
    assert_eq!(page_size(Some(7)), 7);
}

#[test]
fn a_limit_past_the_bound_is_the_whole_log() {
    assert_eq!(page_size(Some(u32::MAX)), MAX_EVENTS_PER_ACCOUNT);
}
