//! The `capsule` argument surface, and the machine-readable description of it that the
//! documentation site is generated from.
//!
//! [`Cli`] stays `pub(crate)`: the parsed command is dispatch state, not API. What crosses
//! the crate boundary is [`command_tree`], the description artifact behind
//! `/reference/cli/` (slice `S-Z8`).

pub(crate) mod commands;
pub mod help;

use clap::{Arg, ArgAction, Command, CommandFactory, Parser};
pub(crate) use commands::*;
use serde_json::{Map, Value};

use crate::i18n::Bundle;

/// Every field name the command-tree document uses, named once.
///
/// The document is hand-built rather than derived from a struct, because its shape is a
/// projection of `clap`'s builder API and no Rust type in this crate has that shape. Naming
/// the keys here is what keeps that from meaning "spelled ad hoc": this block is the
/// vocabulary `capsule-docs/scripts/gen-reference.mjs` reads on the other side, and a typo
/// in a key is a field the generator silently never finds.
mod field {
    pub(super) const SCHEMA: &str = "schema";
    pub(super) const NAME: &str = "name";
    pub(super) const ABOUT: &str = "about";
    pub(super) const LONG_ABOUT: &str = "long_about";
    pub(super) const ARGS: &str = "args";
    pub(super) const SUBCOMMANDS: &str = "subcommands";
    pub(super) const ID: &str = "id";
    pub(super) const POSITIONAL: &str = "positional";
    pub(super) const REQUIRED: &str = "required";
    pub(super) const TAKES_VALUE: &str = "takes_value";
    pub(super) const REPEATABLE: &str = "repeatable";
    pub(super) const LONG: &str = "long";
    pub(super) const SHORT: &str = "short";
    pub(super) const VALUE_NAMES: &str = "value_names";
    pub(super) const POSSIBLE_VALUES: &str = "possible_values";
    pub(super) const DEFAULT_VALUES: &str = "default_values";
    pub(super) const HELP: &str = "help";
    pub(super) const LONG_HELP: &str = "long_help";
}

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
    // Markdown list markers, not `•`: this text is the root `long_about` in the committed
    // command tree, and the reference page renders it as prose. Bullet characters soft-wrap
    // into one run-on paragraph there, while `-` renders as the list it already is. A
    // terminal shows `-` as a list too, so `capsule --help` loses nothing.
    long_about = "Capsule CLI provides tools for managing your photos and albums:\n\n- Authentication management\n- Sync local and remote data\n- Check status and list files\n- Manage albums and collections"
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
/// - **Locale-independent.** Help text is localized (slice `S-I8`, [`help::localize`]), and
///   this function resolves it through an explicitly pinned **`en`** bundle —
///   `Bundle::for_locale("en")`, never [`crate::i18n::cli_bundle`], which negotiates
///   `LC_ALL`/`LC_MESSAGES`/`LANG`. The artifact describes one surface in one language; a
///   bundle negotiated from the environment would make `cli-surface-check` pass or fail
///   according to the developer's `LANG`, and the drift gate would stop meaning anything.
///   `help`'s invariant test additionally proves the `en` entries equal the derive text, so
///   pinning `en` yields the same tree the un-localized derive did. `StyledStr`'s `Display`
///   is documented as colour-unaware, so no ANSI escape can leak in from a terminal that
///   supports colour.
///
/// Localizing the *rendered* help a user sees is a separate concern from describing the
/// surface: the binary's [`crate::run`] applies the same rewriter under the negotiated
/// bundle, and this function does not.
///
/// The tree describes the surface this crate *declares*. `clap`'s generated `--help` (and
/// `--version`, were one configured) is deliberately absent: [`Command::build`] is not
/// called, so no synthesized argument is described, and the reference page does not repeat
/// `--help` under every command. Hidden commands and arguments are skipped for the same
/// reason they are hidden.
#[must_use]
pub fn command_tree() -> Value {
    let mut root = describe_command(&help::localize(Cli::command(), &Bundle::for_locale("en")));
    root.insert(
        field::SCHEMA.to_owned(),
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
    out.insert(field::NAME.to_owned(), Value::from(command.get_name()));

    if let Some(about) = command.get_about() {
        out.insert(field::ABOUT.to_owned(), Value::from(about.to_string()));
    }
    // Emitted only when it says something `about` does not, so the artifact does not carry
    // the same paragraph twice for every command whose doc comment is one line long.
    if let Some(long_about) = command.get_long_about() {
        let long_about = long_about.to_string();
        if Some(long_about.as_str()) != command.get_about().map(ToString::to_string).as_deref() {
            out.insert(field::LONG_ABOUT.to_owned(), Value::from(long_about));
        }
    }

    let args: Vec<Value> = command
        .get_arguments()
        .filter(|arg| !arg.is_hide_set())
        .map(|arg| Value::Object(describe_arg(arg)))
        .collect();
    if !args.is_empty() {
        out.insert(field::ARGS.to_owned(), Value::from(args));
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
        out.insert(field::SUBCOMMANDS.to_owned(), Value::from(described));
    }

    out
}

/// Describe one argument: how it is spelled, whether it takes a value, and what the help
/// says about it.
fn describe_arg(arg: &Arg) -> Map<String, Value> {
    let mut out = Map::new();
    out.insert(field::ID.to_owned(), Value::from(arg.get_id().as_str()));
    out.insert(
        field::POSITIONAL.to_owned(),
        Value::from(arg.is_positional()),
    );
    out.insert(
        field::REQUIRED.to_owned(),
        Value::from(arg.is_required_set()),
    );
    out.insert(field::TAKES_VALUE.to_owned(), Value::from(takes_value(arg)));
    out.insert(
        field::REPEATABLE.to_owned(),
        Value::from(is_repeatable(arg)),
    );

    if let Some(long) = arg.get_long() {
        out.insert(field::LONG.to_owned(), Value::from(long));
    }
    if let Some(short) = arg.get_short() {
        out.insert(field::SHORT.to_owned(), Value::from(short.to_string()));
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
            out.insert(field::VALUE_NAMES.to_owned(), Value::from(names));
        }

        let possible: Vec<Value> = arg
            .get_possible_values()
            .iter()
            .filter(|value| !value.is_hide_set())
            .map(|value| {
                let mut entry = Map::new();
                entry.insert(field::NAME.to_owned(), Value::from(value.get_name()));
                if let Some(help) = value.get_help() {
                    entry.insert(field::HELP.to_owned(), Value::from(help.to_string()));
                }
                Value::Object(entry)
            })
            .collect();
        if !possible.is_empty() {
            out.insert(field::POSSIBLE_VALUES.to_owned(), Value::from(possible));
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
        out.insert(field::DEFAULT_VALUES.to_owned(), Value::from(defaults));
    }

    if let Some(help) = arg.get_help() {
        out.insert(field::HELP.to_owned(), Value::from(help.to_string()));
    }
    if let Some(long_help) = arg.get_long_help() {
        let long_help = long_help.to_string();
        if Some(long_help.as_str()) != arg.get_help().map(ToString::to_string).as_deref() {
            out.insert(field::LONG_HELP.to_owned(), Value::from(long_help));
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
///
/// `Count` is unreachable on today's surface — no argument in this CLI is a `-vvv`-style
/// counter — and is matched anyway because it is the other action that means "give this
/// again", and omitting it would make the first counting flag document itself as single-use.
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

    /// The property the drift gate rests on that no other test reaches: the artifact is
    /// byte-compared, so if any string in it were negotiated from the environment,
    /// `cli-surface-check` would pass or fail according to the developer's `LANG`.
    ///
    /// `LC_ALL` is what `crate::i18n::cli_bundle` reads first, and `tr-TR` is the locale
    /// that breaks case-folding implementations, so between them they exercise both the
    /// negotiation path and the classic locale-sensitivity trap. `nextest` runs each test in
    /// its own process, which is what makes mutating the environment here safe.
    #[test]
    fn the_tree_is_identical_under_two_different_locales() {
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
}
