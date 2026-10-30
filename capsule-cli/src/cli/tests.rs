use super::*;

fn subcommand<'a>(parent: &'a Value, name: &str) -> &'a Value {
    parent
        .get("subcommands")
        .and_then(Value::as_array)
        .expect("the command has subcommands")
        .iter()
        .find(|entry| entry.get("name").and_then(Value::as_str) == Some(name))
        .unwrap_or_else(|| panic!("subcommand `{name}` is described"))
}

fn arg<'a>(command: &'a Value, id: &str) -> &'a Value {
    command
        .get("args")
        .and_then(Value::as_array)
        .expect("the command has arguments")
        .iter()
        .find(|entry| entry.get("id").and_then(Value::as_str) == Some(id))
        .unwrap_or_else(|| panic!("argument `{id}` is described"))
}

/// The property the committed artifact and its `--check` gate rest on. A tree that
/// varied between calls would fail `cli-surface-check` on a tree nobody changed.
#[test]
fn the_tree_is_byte_identical_across_calls() {
    let first = serde_json::to_string_pretty(&command_tree()).expect("the tree serializes");
    let second = serde_json::to_string_pretty(&command_tree()).expect("the tree serializes");
    assert_eq!(first, second);
}

#[test]
fn the_tree_carries_its_schema_version() {
    assert_eq!(
        command_tree().get("schema").and_then(Value::as_u64),
        Some(u64::from(COMMAND_TREE_SCHEMA))
    );
}

/// No terminal escape may reach a committed file: it would make the artifact depend on
/// whether the emitting shell claimed colour support.
#[test]
fn the_tree_carries_no_terminal_escapes() {
    let json = serde_json::to_string(&command_tree()).expect("the tree serializes");
    assert!(
        !json.contains('\u{1b}'),
        "an ANSI escape reached the description artifact"
    );
}

#[test]
fn subcommands_are_sorted_by_name() {
    let tree = command_tree();
    let names: Vec<&str> = tree
        .get("subcommands")
        .and_then(Value::as_array)
        .expect("the root has subcommands")
        .iter()
        .filter_map(|entry| entry.get("name").and_then(Value::as_str))
        .collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted);
    assert!(names.contains(&"auth"));
    assert!(names.contains(&"import"));
}

/// Arguments keep declaration order, which for a positional is its position. Sorting
/// them would silently reorder `capsule library init <path> --name`.
#[test]
fn arguments_keep_declaration_order_so_positionals_stay_in_position() {
    let tree = command_tree();
    let init = subcommand(subcommand(&tree, "library"), "init");
    let ids: Vec<&str> = init
        .get("args")
        .and_then(Value::as_array)
        .expect("`library init` has arguments")
        .iter()
        .filter_map(|entry| entry.get("id").and_then(Value::as_str))
        .collect();
    assert_eq!(ids, vec!["path", "name"]);
    assert_eq!(
        arg(init, "path").get("positional"),
        Some(&Value::from(true))
    );
    assert_eq!(
        arg(init, "name").get("positional"),
        Some(&Value::from(false))
    );
    assert_eq!(
        arg(init, "name").get("default_values"),
        Some(&Value::from(vec![Value::from("My Library")]))
    );
}

#[test]
fn a_flag_is_distinguished_from_a_value_taking_option() {
    let tree = command_tree();
    let import = subcommand(&tree, "import");

    let library = arg(import, "library");
    assert_eq!(library.get("takes_value"), Some(&Value::from(true)));
    assert_eq!(library.get("long"), Some(&Value::from("library")));
    assert_eq!(
        library.get("value_names"),
        Some(&Value::from(vec![Value::from("PATH")]))
    );

    let force = arg(import, "force");
    assert_eq!(force.get("takes_value"), Some(&Value::from(false)));
    assert_eq!(force.get("repeatable"), Some(&Value::from(false)));
    // The derive answers `value_name` and `possible_values` for a flag as well, with
    // its own internals: `FORCE` as a placeholder nobody types, and the `true`/`false`
    // its bool parser accepts. Describing either would invent a surface.
    assert!(force.get("value_names").is_none());
    assert!(force.get("possible_values").is_none());
}

#[test]
fn a_repeatable_argument_is_marked_repeatable() {
    let tree = command_tree();
    let paths = arg(subcommand(&tree, "import"), "paths");
    assert_eq!(paths.get("repeatable"), Some(&Value::from(true)));
    assert_eq!(paths.get("required"), Some(&Value::from(true)));

    let pick = arg(subcommand(&tree, "cull"), "pick");
    assert_eq!(pick.get("repeatable"), Some(&Value::from(true)));
}

#[test]
fn an_enumerated_value_carries_its_variants() {
    let tree = command_tree();
    let provider = arg(subcommand(&tree, "import"), "provider");
    let names: Vec<&str> = provider
        .get("possible_values")
        .and_then(Value::as_array)
        .expect("`--provider` enumerates its values")
        .iter()
        .filter_map(|entry| entry.get("name").and_then(Value::as_str))
        .collect();
    assert_eq!(names, vec!["takeout"]);

    let filter = arg(subcommand(&tree, "cull"), "filter");
    let flags: Vec<&str> = filter
        .get("possible_values")
        .and_then(Value::as_array)
        .expect("`--filter` enumerates its values")
        .iter()
        .filter_map(|entry| entry.get("name").and_then(Value::as_str))
        .collect();
    assert_eq!(flags, vec!["pick", "neutral", "reject"]);
}

/// The property the drift gate rests on that no other test reaches: the artifact is
/// byte-compared, so if any string in it were negotiated from the environment,
/// `cli-surface-check` would pass or fail according to the developer's `LANG`.
///
/// `LC_ALL` is what a POSIX locale negotiation reads first, and `tr-TR` is the locale
/// that breaks case-folding implementations, so between them they would catch both a
/// reintroduced negotiation and the classic locale-sensitivity trap. `nextest` runs each test in
/// its own process, which is what makes mutating the environment here safe.
#[test]
fn the_tree_is_identical_under_two_different_locales() {
    // Captured and put back below. `nextest` gives each test its own process, so this
    // cannot reach another test — but a leaked `LC_ALL=tr_TR` would still be visible to
    // anything else in *this* process, and a test that changes global state and does not
    // change it back is one a reader has to prove harmless every time they see it.
    let saved: Vec<(&str, Option<String>)> = ["LC_ALL", "LANG"]
        .iter()
        .map(|name| (*name, std::env::var(name).ok()))
        .collect();

    let render = |locale: &str| {
        // SAFETY: single-threaded test body in a process nextest gives this test alone.
        unsafe {
            std::env::set_var("LC_ALL", locale);
            std::env::set_var("LANG", locale);
        }
        serde_json::to_string_pretty(&command_tree()).expect("the tree serializes")
    };
    let english = render("en_US.UTF-8");
    let turkish = render("tr_TR.UTF-8");
    let japanese = render("ja_JP.UTF-8");

    for (name, value) in saved {
        // SAFETY: as above.
        unsafe {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }

    assert_eq!(english, turkish);
    assert_eq!(english, japanese);
    // Guards against the whole comparison passing because every render was empty.
    assert!(english.contains("\"name\": \"capsule\""));
}

/// Both branches of the `long_about`/`long_help` dedup: a distinct long form is carried,
/// an identical one is dropped rather than stored twice.
#[test]
fn a_long_form_is_carried_only_when_it_differs_from_the_short_one() {
    let distinct = Command::new("x")
        .about("Short.")
        .long_about("Short.\n\nAnd more.")
        .arg(
            clap::Arg::new("a")
                .long("a")
                .help("Short help.")
                .long_help("Short help.\n\nAnd more."),
        );
    let described = describe_command(&distinct);
    assert_eq!(
        described.get(field::LONG_ABOUT).and_then(Value::as_str),
        Some("Short.\n\nAnd more.")
    );
    let arg_entry = &described
        .get(field::ARGS)
        .and_then(Value::as_array)
        .expect("the command has arguments")[0];
    assert_eq!(
        arg_entry.get(field::LONG_HELP).and_then(Value::as_str),
        Some("Short help.\n\nAnd more.")
    );

    let same = Command::new("x").about("Short.").long_about("Short.").arg(
        clap::Arg::new("a")
            .long("a")
            .help("Short help.")
            .long_help("Short help."),
    );
    let described = describe_command(&same);
    assert!(described.get(field::LONG_ABOUT).is_none());
    assert!(
        described
            .get(field::ARGS)
            .and_then(Value::as_array)
            .expect("the command has arguments")[0]
            .get(field::LONG_HELP)
            .is_none()
    );
}

/// `Command::build` is deliberately not called, so the artifact describes only what
/// this crate declares — `--help` is not repeated under every command.
#[test]
fn the_tree_omits_claps_synthesized_help_argument() {
    let tree = command_tree();
    assert!(subcommand(&tree, "status").get("args").is_none());
    assert!(
        !serde_json::to_string(&tree)
            .expect("the tree serializes")
            .contains("\"id\":\"help\"")
    );
}

/// On a terminal, bare `capsule` parses to no command: the TUI's cue.
#[test]
fn bare_capsule_on_a_terminal_parses_to_no_command() {
    let cli = try_parse_from(true, ["capsule"]).expect("bare capsule parses on a terminal");
    assert!(cli.command.is_none());
}

/// Off a terminal, bare `capsule` is clap's help-on-missing-subcommand usage error: help on
/// stderr and exit status 2, never a full-screen UI in a pipeline.
#[test]
fn bare_capsule_off_a_terminal_is_the_help_usage_error() {
    let error = try_parse_from(false, ["capsule"]).expect_err("bare capsule is refused");
    assert_eq!(
        error.kind(),
        clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
    );
    assert_eq!(error.exit_code(), 2);
}

/// `capsule push` takes the path snapshot from its flags, and each maps onto the SDK's sample.
#[test]
fn push_takes_the_path_snapshot_from_flags() {
    use capsule_sdk::net::path::{PathCost, PathInterface, PowerState, ThermalState};

    let cli = try_parse_from(
        false,
        [
            "capsule",
            "push",
            "--library",
            "lib",
            "--path-cost",
            "metered",
            "--path-interface",
            "cellular",
            "--power",
            "low-power",
            "--thermal",
            "serious",
            "--background-deadline",
            "30",
        ],
    )
    .expect("parses");
    let Some(Commands::Push { path, .. }) = cli.command else {
        panic!("a push");
    };
    assert_eq!(
        path.flags(),
        crate::cache::PathFlags {
            cost: Some(PathCost::Metered),
            interface: Some(PathInterface::Cellular),
            power: Some(PowerState::LowPower),
            thermal: Some(ThermalState::Serious),
            background_deadline: Some(std::time::Duration::from_secs(30)),
        }
    );

    let bare = try_parse_from(false, ["capsule", "push", "--library", "lib"]).expect("parses");
    let Some(Commands::Push { path, .. }) = bare.command else {
        panic!("a push");
    };
    assert_eq!(path.flags(), crate::cache::PathFlags::default());
}

/// `capsule auth activity` takes an optional limit and the `--json` switch.
#[test]
fn auth_activity_parses_its_limit_and_json() {
    let cli = try_parse_from(
        false,
        ["capsule", "auth", "activity", "--limit", "5", "--json"],
    )
    .expect("parses");
    assert!(matches!(
        cli.command,
        Some(Commands::Auth {
            command: AuthCommands::Activity {
                limit: Some(5),
                json: true
            }
        })
    ));
    let bare = try_parse_from(false, ["capsule", "auth", "activity"]).expect("parses");
    assert!(matches!(
        bare.command,
        Some(Commands::Auth {
            command: AuthCommands::Activity {
                limit: None,
                json: false
            }
        })
    ));
}

/// A subcommand parses the same whether or not a terminal is attached.
#[test]
fn a_subcommand_parses_the_same_on_and_off_a_terminal() {
    for interactive in [true, false] {
        let cli = try_parse_from(interactive, ["capsule", "auth", "status"]).expect("parses");
        assert!(matches!(
            cli.command,
            Some(Commands::Auth {
                command: AuthCommands::Status { json: false }
            })
        ));
    }
}
