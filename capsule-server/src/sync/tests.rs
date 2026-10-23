use super::*;

#[test]
fn a_page_size_is_clamped_rather_than_refused() {
    assert_eq!(clamp_page_size(None), DEFAULT_PAGE_SIZE);
    assert_eq!(clamp_page_size(Some(0)), 1);
    assert_eq!(clamp_page_size(Some(7)), 7);
    assert_eq!(clamp_page_size(Some(MAX_PAGE_SIZE)), MAX_PAGE_SIZE);
    assert_eq!(clamp_page_size(Some(usize::MAX)), MAX_PAGE_SIZE);
}
