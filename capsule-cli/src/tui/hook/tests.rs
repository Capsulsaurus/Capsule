//! The hook is process-global, so the whole behaviour is one test.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

#[test]
fn only_a_panic_on_the_owner_thread_restores_the_terminal() {
    let restored = Arc::new(AtomicUsize::new(0));
    let prior_ran = Arc::new(AtomicUsize::new(0));
    let (r, p) = (Arc::clone(&restored), Arc::clone(&prior_ran));
    let previous = panic::take_hook();
    panic::set_hook(owner_thread_hook(
        thread::current().id(),
        move || {
            r.fetch_add(1, Ordering::SeqCst);
        },
        Box::new(move |_| {
            p.fetch_add(1, Ordering::SeqCst);
        }),
    ));

    let contained = thread::spawn(|| panic!("a port panicked")).join();
    assert!(contained.is_err());
    assert_eq!(
        restored.load(Ordering::SeqCst),
        0,
        "other thread: no restore"
    );
    assert_eq!(prior_ran.load(Ordering::SeqCst), 0);

    let own = panic::catch_unwind(|| panic!("the loop panicked"));
    assert!(own.is_err());
    assert_eq!(restored.load(Ordering::SeqCst), 1, "owner thread: restored");
    assert_eq!(prior_ran.load(Ordering::SeqCst), 1);

    panic::set_hook(previous);
}
