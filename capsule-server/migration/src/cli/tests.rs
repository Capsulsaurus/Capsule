use clap::{CommandFactory as _, Parser as _};

use super::{Cli, Command};

fn parse(args: &[&str]) -> Cli {
    Cli::try_parse_from(
        ["capsule-server-migration", "-u", "postgres://db"]
            .iter()
            .chain(args),
    )
    .unwrap()
}

#[test]
fn the_command_line_definition_is_consistent() {
    Cli::command().debug_assert();
}

/// The documented upgrade step, `capsule-server-migration up`, applies everything pending.
#[test]
fn up_applies_everything_by_default() {
    assert_eq!(parse(&["up"]).operation(), Command::Up { num: None });
}

#[test]
fn no_subcommand_means_up() {
    assert_eq!(parse(&[]).operation(), Command::Up { num: None });
}

#[test]
fn up_takes_a_step_count() {
    assert_eq!(
        parse(&["up", "-n", "2"]).operation(),
        Command::Up { num: Some(2) }
    );
}

/// `down` rolls back one migration unless told otherwise: a bare `down` never empties a schema.
#[test]
fn down_rolls_back_one_by_default() {
    assert_eq!(parse(&["down"]).operation(), Command::Down { num: 1 });
    assert_eq!(
        parse(&["down", "--num", "3"]).operation(),
        Command::Down { num: 3 }
    );
}

#[test]
fn status_is_a_command() {
    assert_eq!(parse(&["status"]).operation(), Command::Status);
}

/// The destructive verbs `sea-orm-migration` offered are not accepted.
#[test]
fn fresh_refresh_and_reset_are_refused() {
    for verb in ["fresh", "refresh", "reset"] {
        assert!(
            Cli::try_parse_from(["capsule-server-migration", "-u", "postgres://db", verb]).is_err(),
            "{verb} was accepted"
        );
    }
}
