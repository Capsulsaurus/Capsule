//! The `capsule` argument surface, and the machine-readable description of it that the
//! documentation site is generated from.
//!
//! [`Cli`] stays `pub(crate)`: the parsed command is dispatch state, not API. What crosses
//! the crate boundary is [`command_tree`], the description artifact behind
//! `/reference/cli/` (slice `S-Z8`).

pub(crate) mod commands;

use clap::{Arg, ArgAction, Command, CommandFactory, Parser};
pub(crate) use commands::*;
use serde_json::{Map, Value};

/// Schema version of the emitted command-tree document.
///
/// Bumped only when a consumer must change to keep reading it — adding an optional field is
/// not a bump, renaming or removing one is. `capsule-docs/scripts/gen-reference.mjs` refuses
/// a version it was not written against rather than rendering a half-understood document.
const COMMAND_TREE_SCHEMA: u32 = 1;

#[derive(Parser, Debug)]
#[command(name = "capsule")]
#[command(about = "A command line interface for Capsule - the photo management platform")]
#[command(
    long_about = "Capsule CLI provides tools for managing your photos and albums:\n• Authentication management\n• Sync local and remote data\n• Check status and list files\n• Manage albums and collections"
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

/// The `capsule` command tree as JSON — the committed description artifact
/// `capsule-cli/cli-surface.json`, emitted by the `gen_cli_surface` binary and rendered
/// into `/reference/cli/` by the documentation build (slice `S-Z8`).
///
/// # This output is deterministic, and independent of the process locale
///
/// Both properties are load-bearing, because the artifact is committed and drift-gated:
/// `mise run cli-surface-check` fails on any byte difference, so a value that varies by
/// machine, environment, or run would fail CI on a tree nobody changed.
///
/// - **Deterministic.** Subcommands are sorted by name rather than emitted in
///   declaration order, so reordering an enum variant cannot churn the artifact.
///   Arguments keep declaration order, which for a `clap` derive is field order, which
///   for a positional *is* its position — sorting them would destroy that meaning.
///   Object keys come out sorted because `serde_json::Map` is a `BTreeMap` here
///   (`preserve_order` is off). Nothing is read from the clock, the filesystem, or the
///   environment.
/// - **Locale-independent.** Every string below comes from a `clap` attribute or a doc
///   comment — compile-time English `&'static str` — and `StyledStr`'s `Display` is
///   documented as colour-unaware, so no ANSI escape can leak in from a terminal that
///   supports colour. The process locale is never consulted: this function does **not**
///   call [`crate::i18n::cli_bundle`], which negotiates `LC_ALL`/`LC_MESSAGES`/`LANG`.
///
/// **If help text is ever localized, it must be resolved here through
/// `Bundle::for_locale("en")`, never through `cli_bundle()`.** The artifact describes one
/// surface in one language; a bundle negotiated from the environment would make
/// `cli-surface-check` pass or fail according to the developer's `LANG`, and the drift
/// gate would stop meaning anything. Localizing the *rendered* help a user sees is a
/// separate concern from describing the surface.
///
/// The tree describes the surface this crate *declares*. `clap`'s generated `--help` (and
/// `--version`, were one configured) is deliberately absent: [`Command::build`] is not
/// called, so no synthesized argument is described, and the reference page does not repeat
/// `--help` under all sixteen commands. Hidden commands and arguments are skipped for the
/// same reason they are hidden.
#[must_use]
pub fn command_tree() -> Value {
    let mut root = describe_command(&Cli::command());
    root.insert(
        "schema".to_owned(),
        Value::from(u64::from(COMMAND_TREE_SCHEMA)),
    );
    Value::Object(root)
}

/// Describe one command and, recursively, its subcommands.
///
/// Recursion is bounded by the derive: the tree is a finite `enum` nesting, so there is no
/// cycle to guard against and no depth limit to pick.
fn describe_command(command: &Command) -> Map<String, Value> {
    let mut out = Map::new();
    out.insert("name".to_owned(), Value::from(command.get_name()));

    if let Some(about) = command.get_about() {
        out.insert("about".to_owned(), Value::from(about.to_string()));
    }
    // Emitted only when it says something `about` does not, so the artifact does not carry
    // the same paragraph twice for every command whose doc comment is one line long.
    if let Some(long_about) = command.get_long_about() {
        let long_about = long_about.to_string();
        if Some(long_about.as_str()) != command.get_about().map(ToString::to_string).as_deref() {
            out.insert("long_about".to_owned(), Value::from(long_about));
        }
    }

    let args: Vec<Value> = command
        .get_arguments()
        .filter(|arg| !arg.is_hide_set())
        .map(|arg| Value::Object(describe_arg(arg)))
        .collect();
    if !args.is_empty() {
        out.insert("args".to_owned(), Value::from(args));
    }

    let mut subcommands: Vec<&Command> = command
        .get_subcommands()
        .filter(|subcommand| !subcommand.is_hide_set())
        .collect();
    subcommands.sort_by(|a, b| a.get_name().cmp(b.get_name()));
    if !subcommands.is_empty() {
        let described: Vec<Value> = subcommands
            .into_iter()
            .map(|subcommand| Value::Object(describe_command(subcommand)))
            .collect();
        out.insert("subcommands".to_owned(), Value::from(described));
    }

    out
}

/// Describe one argument: how it is spelled, whether it takes a value, and what the help
/// says about it.
fn describe_arg(arg: &Arg) -> Map<String, Value> {
    let mut out = Map::new();
    out.insert("id".to_owned(), Value::from(arg.get_id().as_str()));
    out.insert("positional".to_owned(), Value::from(arg.is_positional()));
    out.insert("required".to_owned(), Value::from(arg.is_required_set()));
    out.insert("takes_value".to_owned(), Value::from(takes_value(arg)));
    out.insert("repeatable".to_owned(), Value::from(is_repeatable(arg)));

    if let Some(long) = arg.get_long() {
        out.insert("long".to_owned(), Value::from(long));
    }
    if let Some(short) = arg.get_short() {
        out.insert("short".to_owned(), Value::from(short.to_string()));
    }
    // Both of these are asked only of a value-taking argument, because the derive answers
    // them for a flag too and both answers are internal detail rather than surface. A
    // `bool` field gets a `value_name` synthesized from its identifier (`PASSWORD_STDIN`)
    // that no user may type, and `get_possible_values` reports the `true`/`false` its bool
    // parser accepts — rendering either would document `--password-stdin <PASSWORD_STDIN>`
    // taking `true` or `false`, which is not the flag `capsule --help` offers.
    if takes_value(arg) {
        if let Some(names) = arg.get_value_names() {
            let names: Vec<Value> = names
                .iter()
                .map(|name| Value::from(name.to_string()))
                .collect();
            out.insert("value_names".to_owned(), Value::from(names));
        }

        let possible: Vec<Value> = arg
            .get_possible_values()
            .iter()
            .filter(|value| !value.is_hide_set())
            .map(|value| {
                let mut entry = Map::new();
                entry.insert("name".to_owned(), Value::from(value.get_name()));
                if let Some(help) = value.get_help() {
                    entry.insert("help".to_owned(), Value::from(help.to_string()));
                }
                Value::Object(entry)
            })
            .collect();
        if !possible.is_empty() {
            out.insert("possible_values".to_owned(), Value::from(possible));
        }
    }

    // `OsStr` here is `clap`'s, which derefs to the standard one. Every default in this
    // surface is ASCII, so the lossy conversion is exact; a non-UTF-8 default would be
    // unrepresentable in JSON either way.
    let defaults: Vec<Value> = arg
        .get_default_values()
        .iter()
        .map(|value| Value::from(value.to_string_lossy().into_owned()))
        .collect();
    if !defaults.is_empty() {
        out.insert("default_values".to_owned(), Value::from(defaults));
    }

    if let Some(help) = arg.get_help() {
        out.insert("help".to_owned(), Value::from(help.to_string()));
    }
    if let Some(long_help) = arg.get_long_help() {
        let long_help = long_help.to_string();
        if Some(long_help.as_str()) != arg.get_help().map(ToString::to_string).as_deref() {
            out.insert("long_help".to_owned(), Value::from(long_help));
        }
    }

    out
}

/// Whether the argument consumes a value (`--library <PATH>`) rather than being a flag
/// (`--force`).
///
/// Read from the action rather than from `num_args`, which the derive leaves unset unless
/// a `#[arg(num_args = …)]` says otherwise. `ArgAction` is `#[non_exhaustive]`, so a new
/// value-taking action would arrive here as `false` — visible as a missing placeholder on
/// the reference page, never as a wrong one.
fn takes_value(arg: &Arg) -> bool {
    matches!(arg.get_action(), ArgAction::Set | ArgAction::Append)
}

/// Whether the argument may be given more than once (`--pick <ID> --pick <ID>`).
fn is_repeatable(arg: &Arg) -> bool {
    matches!(arg.get_action(), ArgAction::Append | ArgAction::Count)
        || arg
            .get_num_args()
            .is_some_and(|range| range.max_values() > 1)
}

#[cfg(test)]
mod tests {
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
}
