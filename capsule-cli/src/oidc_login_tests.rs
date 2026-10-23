use capsule_sdk::auth::AuthError;
use clap::Parser as _;

use super::{describe_oidc_error, device_waiting_line, oidc_waiting_line};
use crate::cli::{AuthCommands, Cli, Commands};
use crate::i18n::{error_codes, error_message};
use crate::oidc::OidcLoginError;
use crate::remote::RemoteError;

/// `--oidc` is its own sign-in: it takes no password inputs, and `--no-browser` means
/// nothing without it.
#[test]
fn the_oidc_flag_excludes_the_password_inputs() {
    let parsed = Cli::try_parse_from(["capsule", "auth", "login", "--oidc", "--no-browser"])
        .expect("--oidc --no-browser parses");
    assert!(matches!(
        parsed.command.expect("a subcommand was given"),
        Commands::Auth {
            command: AuthCommands::Login {
                oidc: true,
                no_browser: true,
                ..
            }
        }
    ));
    for refused in [
        &["capsule", "auth", "login", "--oidc", "--email", "a@b.c"][..],
        &["capsule", "auth", "login", "--oidc", "--password-stdin"][..],
        &["capsule", "auth", "login", "--no-browser"][..],
    ] {
        assert!(Cli::try_parse_from(refused).is_err(), "{refused:?}");
    }
}

/// `--device` is an arm of `--oidc` and opens no browser, so `--no-browser` has nothing to
/// add to it (issue #635).
#[test]
fn the_device_flag_rides_oidc_alone() {
    let parsed = Cli::try_parse_from(["capsule", "auth", "login", "--oidc", "--device"])
        .expect("--oidc --device parses");
    assert!(matches!(
        parsed.command.expect("a subcommand was given"),
        Commands::Auth {
            command: AuthCommands::Login {
                oidc: true,
                device: true,
                no_browser: false,
                ..
            }
        }
    ));
    for refused in [
        &["capsule", "auth", "login", "--device"][..],
        &[
            "capsule",
            "auth",
            "login",
            "--oidc",
            "--device",
            "--no-browser",
        ][..],
        &["capsule", "auth", "login", "--device", "--email", "a@b.c"][..],
    ] {
        assert!(Cli::try_parse_from(refused).is_err(), "{refused:?}");
    }
}

/// The device sign-in's own failures render their own lines: the timeout, and the server's
/// denial, expiry and unsupported answers through their catalog codes.
#[test]
fn each_device_failure_has_its_own_line() {
    let timed_out = describe_oidc_error(&OidcLoginError::DeviceTimedOut {
        waited: std::time::Duration::from_secs(600),
    });
    assert!(timed_out.contains("not approved in time"), "{timed_out}");
    for (error, code) in [
        (
            AuthError::OidcDeviceDenied,
            error_codes::AUTH_OIDC_DEVICE_DENIED,
        ),
        (
            AuthError::OidcDeviceExpired,
            error_codes::AUTH_OIDC_DEVICE_EXPIRED,
        ),
        (
            AuthError::OidcDeviceUnsupported,
            error_codes::AUTH_OIDC_DEVICE_UNSUPPORTED,
        ),
    ] {
        assert_eq!(
            describe_oidc_error(&OidcLoginError::Remote(RemoteError::Auth(error))),
            error_message(code)
        );
    }
}

/// A refused loopback redirect names the server variable the operator has to set.
#[test]
fn a_refused_loopback_redirect_names_the_server_setting() {
    let line = describe_oidc_error(&OidcLoginError::Remote(RemoteError::Auth(
        AuthError::OidcRedirectInvalid,
    )));
    assert!(line.contains("OIDC_ALLOW_LOOPBACK_REDIRECT=true"), "{line}");
}

/// Each terminal failure renders its own line, and provider text cannot carry control
/// characters into the terminal.
#[test]
fn each_oidc_failure_has_its_own_line() {
    let timed_out = describe_oidc_error(&OidcLoginError::TimedOut {
        waited: std::time::Duration::from_secs(300),
    });
    assert!(timed_out.contains("No sign-in came back"), "{timed_out}");
    assert_eq!(
        describe_oidc_error(&OidcLoginError::Denied {
            error: "access_denied".to_owned(),
            description: None,
        }),
        "The identity provider did not sign you in (access_denied)."
    );
    let described = describe_oidc_error(&OidcLoginError::Denied {
        error: "access_\u{1b}denied".to_owned(),
        description: Some("no\u{1b}[31m thanks".to_owned()),
    });
    assert_eq!(
        described,
        "The identity provider did not sign you in (access_denied): no[31m thanks"
    );
    assert!(
        describe_oidc_error(&OidcLoginError::Remote(RemoteError::Auth(
            AuthError::OidcNotConfigured
        )))
        .contains(&error_message(error_codes::AUTH_OIDC_NOT_CONFIGURED))
    );
}

/// The wait lines pluralize on whole minutes, rounded up.
#[test]
fn the_wait_lines_count_minutes() {
    let secs = std::time::Duration::from_secs;
    assert_eq!(
        oidc_waiting_line(secs(60)),
        "Waiting up to 1 minute for the sign-in to finish…"
    );
    assert_eq!(
        oidc_waiting_line(secs(241)),
        "Waiting up to 5 minutes for the sign-in to finish…"
    );
    assert_eq!(
        device_waiting_line(secs(30)),
        "Waiting up to 1 minute for you to approve the sign-in…"
    );
    assert_eq!(
        device_waiting_line(secs(600)),
        "Waiting up to 10 minutes for you to approve the sign-in…"
    );
}
