//! Log-routing tests. One test drives the whole lifecycle, because the routing target is
//! process-global and two tests flipping it would race under a threaded test runner.

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
    drop(redirect);
    assert!(target().is_none(), "back to stderr after the guard drops");

    assert_eq!(
        std::fs::read_to_string(&path).expect("read log"),
        "while the tui runs\n"
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
