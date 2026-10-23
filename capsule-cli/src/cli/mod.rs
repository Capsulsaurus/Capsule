//! The `capsule` argument surface, and the machine-readable description of it that the
//! documentation site is generated from.
//!
//! `Cli` stays `pub(crate)`: the parsed command is dispatch state, not API. What crosses
//! the crate boundary is [`command_tree`], the description artifact behind
//! `/reference/cli/`.

pub(crate) mod commands;

use clap::{Arg, ArgAction, Command, CommandFactory, Parser};
pub(crate) use commands::*;
use serde_json::{Map, Value};

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
    /// The command to run. Absent only for a bare `capsule`, which opens the TUI on a terminal
    /// and prints this help anywhere else ([`parse`]).
    #[command(subcommand)]
    pub command: Option<Commands>,
}

/// Parse the process arguments.
///
/// Bare `capsule` gets through only when `interactive` (stdin and stdout are terminals), and the
/// caller opens the TUI for it. Off a terminal it is what a missing subcommand always was for a
/// script: clap's help on stderr and exit status 2, so a pipeline never blocks on a full-screen
/// UI.
pub(crate) fn parse(interactive: bool) -> Cli {
    try_parse_from(interactive, std::env::args_os()).unwrap_or_else(|e| e.exit())
}

/// [`parse`] over explicit arguments, returning the usage error instead of exiting.
pub(crate) fn try_parse_from<I, T>(interactive: bool, args: I) -> Result<Cli, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    use clap::FromArgMatches as _;
    let matches = Cli::command()
        .arg_required_else_help(!interactive)
        .try_get_matches_from(args)?;
    Cli::from_arg_matches(&matches)
}

/// The `capsule` command tree as JSON — the committed description artifact
/// `capsule-cli/cli-surface.json`, emitted by the `gen_cli_surface` binary and rendered
/// into `/reference/cli/` by the documentation build.
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
/// - **Locale-independent.** The CLI is English-only: help text is the derive's doc comments
///   and `#[arg]` attributes, verbatim, and nothing reads `LC_ALL`/`LC_MESSAGES`/`LANG`. A
///   string negotiated from the environment would make `cli-surface-check` pass or fail
///   according to the developer's `LANG`, and the drift gate would stop meaning anything.
///   `StyledStr`'s `Display` is documented as colour-unaware, so no ANSI escape can leak in
///   from a terminal that supports colour.
///
/// The tree describes the surface this crate *declares*. `clap`'s generated `--help` (and
/// `--version`, were one configured) is deliberately absent: [`Command::build`] is not
/// called, so no synthesized argument is described, and the reference page does not repeat
/// `--help` under every command. Hidden commands and arguments are skipped for the same
/// reason they are hidden.
#[must_use]
pub fn command_tree() -> Value {
    let mut root = describe_command(&Cli::command());
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
mod tests;
