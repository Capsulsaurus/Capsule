use super::read_stdin_secret;

/// Only the trailing newline is trimmed, so inner and leading spaces survive.
#[test]
fn the_first_line_is_read_with_only_its_newline_trimmed() {
    let mut input = std::io::Cursor::new(" two words \r\nsecond line\n");
    let secret = read_stdin_secret(&mut input, "--password-stdin").unwrap();
    assert_eq!(secret, " two words ");
}

/// An empty line is refused against the flag the command was given, not a fixed one:
/// `capsule auth recover` has no `--password-stdin` (#618 D2).
#[test]
fn an_empty_line_names_the_flag_that_asked_for_it() {
    for flag in ["--recovery-secret-stdin", "--passphrase-stdin"] {
        let mut input = std::io::Cursor::new("\n");
        let err = read_stdin_secret(&mut input, flag).unwrap_err().to_string();
        assert!(err.starts_with(flag), "{flag}: got {err}");
        assert!(!err.contains("--password-stdin"), "{flag}: got {err}");
    }
}

/// End of input with nothing read is the same refusal as an empty line.
#[test]
fn no_input_at_all_is_refused() {
    let mut input = std::io::Cursor::new("");
    let err = read_stdin_secret(&mut input, "--passphrase-stdin").unwrap_err();
    assert!(err.to_string().contains("--passphrase-stdin"));
}
