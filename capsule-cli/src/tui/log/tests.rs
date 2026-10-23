//! Log-routing tests. One test drives the whole lifecycle, because the routing target is
//! process-global and two tests flipping it would race under a threaded test runner.

use tracing_subscriber::prelude::*;

use super::*;

#[test]
fn a_redirect_sends_events_to_its_file_until_it_drops() {
    let dir = std::env::temp_dir().join(format!("capsule-tui-log-{}", nanoid::nanoid!()));
    let path = dir.join("nested").join("tui.log");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    // A stale file from an earlier session is replaced, not appended to.
    std::fs::create_dir_all(path.parent().expect("parent")).expect("parent dir");
    std::fs::write(&path, b"stale\n").expect("seed stale log");

    assert!(target().is_none(), "stderr until redirected");
    let redirect = redirect_to(&path).expect("open log");
    {
        let mut sink = LogWriter.make_writer();
        sink.write_all(b"while the tui runs\n").expect("write");
        sink.flush().expect("flush");
    }
    assert!(is_redirected());
    // An event emitted while redirected lands in the file without colour codes.
    let subscriber = tracing_subscriber::registry().with(layers());
    tracing::subscriber::with_default(subscriber, || {
        tracing::warn!("coloured on a terminal only");
    });
    drop(redirect);
    assert!(target().is_none(), "back to stderr after the guard drops");
    assert!(!is_redirected());

    let logged = std::fs::read_to_string(&path).expect("read log");
    assert!(logged.starts_with("while the tui runs\n"), "{logged}");
    assert!(logged.contains("coloured on a terminal only"), "{logged}");
    assert!(
        !logged.contains('\u{1b}'),
        "no ANSI escapes in the file: {logged:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_redirect_to_an_unwritable_path_fails_and_leaves_stderr() {
    let dir = std::env::temp_dir().join(format!("capsule-tui-log-{}", nanoid::nanoid!()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    // The path is a directory, so it cannot be created as a file.
    assert!(redirect_to(&dir).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
