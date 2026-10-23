use super::*;

#[test]
fn test_cancel_and_check() {
    let token = CancellationToken::new();
    assert!(!token.is_cancelled());
    token.cancel();
    assert!(token.is_cancelled());
}

#[test]
fn test_clone_shares_state() {
    let token = CancellationToken::new();
    let cloned = token.clone();
    token.cancel();
    assert!(cloned.is_cancelled(), "clone should see cancellation");
}
