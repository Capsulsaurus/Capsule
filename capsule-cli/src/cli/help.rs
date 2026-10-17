//! Localized `--help` text (slice `S-I8`).
//!
//! `clap`'s derive renders help from doc comments and `#[arg]` attributes — compile-time
//! English with no seam a catalog key could pass through. This module is that seam: a
//! rewriter that walks a built [`Command`] tree and replaces every `about`, `long_about`,
//! `help` and `long_help` with the message the catalog holds under a key derived from the
//! command's position in the tree. The doc comments stay where they are and keep being the
//! source the catalog's `en` entries are checked against, so there is exactly one place the
//! English is authored and one test that proves the catalog has not drifted from it.
//!
//! ## The key grammar
//!
//! One key per help string, derived from the command path and the argument id — the derive
//! field name — so nothing is spelled twice:
//!
//! | Key | Text it replaces |
//! | --- | --- |
//! | `cli.help.root.about` | the root command's `about` |
//! | `cli.help.root.long_about` | the root command's `long_about` |
//! | `cli.help.<path>.about` | a subcommand's `about`, e.g. `cli.help.library.init.about` |
//! | `cli.help.<path>.long_about` | its `long_about`, when the derive gives it a distinct one |
//! | `cli.help.<path>.arg.<id>` | an argument's `help`, e.g. `cli.help.import.arg.library` |
//! | `cli.help.<path>.arg.<id>.long_help` | its `long_help`, when the derive gives it a distinct one |
//!
//! `<path>` is the dot-joined chain of subcommand names below the root; the root itself is
//! spelled `root`, a name no subcommand may take.
//!
//! ## Two properties the rest of the crate relies on
//!
//! - **A missing key changes nothing.** The look-up is [`Bundle::message`], which reports a
//!   miss as `None` rather than as the key, and a miss leaves the derive text in place. So a
//!   locale with no `cli.help.*` entries yet renders today's English, never a raw key in a
//!   terminal — and a locale with *some* entries renders a mix, which is what partial
//!   translation is supposed to look like.
//! - **Under the source locale the tree is unchanged.** Every `en` entry is asserted equal to
//!   the derive text it replaces, so `capsule --help` under `LANG=C` is byte-identical to
//!   the un-localized rendering, and [`command_tree`](super::command_tree) — which resolves
//!   through an explicitly pinned `en` bundle — emits the same `cli-surface.json` it did
//!   before help was localized.
//!
//! What this cannot reach is a `ValueEnum` variant's help (`--filter pick` → "A keeper."):
//! `clap` 4.6 re-words a possible value only by replacing the typed `value_parser` with a
//! `PossibleValuesParser`, which would trade typed parsing for a translated word. That gap is
//! recorded in the i18n design doc rather than closed here.

use clap::{Arg, Command};

use crate::i18n::{Bundle, HELP_NAMESPACE};

/// The path segment naming the root command in a help key.
pub const ROOT_PATH: &str = "root";

/// The key holding a command's one-line description.
#[must_use]
pub fn about_key(path: &str) -> String {
    format!("{HELP_NAMESPACE}.{path}.about")
}

/// The key holding a command's long description (shown by `--help`, not `-h`).
#[must_use]
pub fn long_about_key(path: &str) -> String {
    format!("{HELP_NAMESPACE}.{path}.long_about")
}

/// The key holding an argument's help line.
#[must_use]
pub fn arg_key(path: &str, arg_id: &str) -> String {
    format!("{HELP_NAMESPACE}.{path}.arg.{arg_id}")
}

/// The key holding an argument's long help (shown by `--help`, not `-h`).
#[must_use]
pub fn arg_long_help_key(path: &str, arg_id: &str) -> String {
    format!("{HELP_NAMESPACE}.{path}.arg.{arg_id}.long_help")
}

/// The path of `name` when it is a subcommand of the command at `parent`.
#[must_use]
pub fn child_path(parent: &str, name: &str) -> String {
    if parent == ROOT_PATH {
        name.to_owned()
    } else {
        format!("{parent}.{name}")
    }
}

/// Replace every help string in `command` (recursively) with the message `bundle` holds for
/// it, leaving any string whose key the bundle lacks exactly as the derive wrote it.
#[must_use]
pub fn localize(command: Command, bundle: &Bundle) -> Command {
    localize_with(command, &|key| bundle.message(key).map(str::to_owned))
}

/// [`localize`] over an arbitrary look-up, so the rewriter is testable against a stub that
/// answers for one key and nothing else.
#[must_use]
pub fn localize_with(command: Command, lookup: &dyn Fn(&str) -> Option<String>) -> Command {
    localize_at(command, ROOT_PATH, lookup)
}

fn localize_at(command: Command, path: &str, lookup: &dyn Fn(&str) -> Option<String>) -> Command {
    let command = localize_command_text(command, path, lookup);
    let command = command.mut_args(|arg| localize_arg(arg, path, lookup));
    command.mut_subcommands(|subcommand| {
        let child = child_path(path, subcommand.get_name());
        localize_at(subcommand, &child, lookup)
    })
}

/// Rewrite `about` / `long_about`. The long form is kept in step with the short one: the
/// derive sets both to the same text for a one-paragraph doc comment, and replacing only the
/// short form would make `-h` speak one language and `--help` another.
fn localize_command_text(
    mut command: Command,
    path: &str,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Command {
    let derive_about = command.get_about().map(ToString::to_string);
    let derive_long = command.get_long_about().map(ToString::to_string);
    if let Some(about) = lookup(&about_key(path)) {
        if derive_long.is_some() && derive_long == derive_about {
            command = command.long_about(about.clone());
        }
        command = command.about(about);
    }
    if let Some(long_about) = lookup(&long_about_key(path)) {
        command = command.long_about(long_about);
    }
    command
}

/// Rewrite `help` / `long_help`, with the same lockstep rule as the command text.
fn localize_arg(mut arg: Arg, path: &str, lookup: &dyn Fn(&str) -> Option<String>) -> Arg {
    let id = arg.get_id().as_str().to_owned();
    let derive_help = arg.get_help().map(ToString::to_string);
    let derive_long = arg.get_long_help().map(ToString::to_string);
    if let Some(help) = lookup(&arg_key(path, &id)) {
        if derive_long.is_some() && derive_long == derive_help {
            arg = arg.long_help(help.clone());
        }
        arg = arg.help(help);
    }
    if let Some(long_help) = lookup(&arg_long_help_key(path, &id)) {
        arg = arg.long_help(long_help);
    }
    arg
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;
    use crate::cli::Cli;

    /// Every `(path, command)` pair in the tree, root first.
    fn commands(command: &Command, path: &str, out: &mut Vec<(String, Command)>) {
        out.push((path.to_owned(), command.clone()));
        for subcommand in command.get_subcommands() {
            commands(subcommand, &child_path(path, subcommand.get_name()), out);
        }
    }

    /// `--help` of every command in the tree, rendered in tree order.
    fn rendered_help(command: &Command) -> Vec<String> {
        let mut all = Vec::new();
        commands(command, ROOT_PATH, &mut all);
        all.into_iter()
            .map(|(_, mut command)| command.render_long_help().to_string())
            .collect()
    }

    /// The invariant `cli-surface.json` and `capsule --help` rest on: the source catalog's
    /// entry for every help string is the derive text, verbatim — so localizing under `en`
    /// is the identity, and a doc comment edited without its catalog entry (or the reverse)
    /// fails here rather than shipping two Englishes.
    #[test]
    fn every_help_string_has_an_english_catalog_entry_equal_to_the_derive_text() {
        let bundle = Bundle::for_locale("en");
        let mut all = Vec::new();
        commands(&Cli::command(), ROOT_PATH, &mut all);
        assert!(all.len() > 16, "the whole tree is walked");

        // The converse: every `cli.help.*` key in the canonical catalog is one the walk
        // produces, so a key left behind by a renamed command or argument fails here too.
        let catalog: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../locales/en.json"),
            )
            .expect("the canonical catalog is readable"),
        )
        .expect("the canonical catalog is JSON");
        let mut produced = std::collections::BTreeSet::new();
        for (path, command) in &all {
            produced.insert(about_key(path));
            produced.insert(long_about_key(path));
            for arg in command.get_arguments() {
                let id = arg.get_id().as_str();
                produced.insert(arg_key(path, id));
                produced.insert(arg_long_help_key(path, id));
            }
        }
        let dead: Vec<&String> = catalog
            .as_object()
            .expect("the catalog is an object")
            .keys()
            .filter(|key| key.starts_with(&format!("{HELP_NAMESPACE}.")))
            .filter(|key| !produced.contains(key.as_str()))
            .collect();
        assert!(
            dead.is_empty(),
            "catalog keys no command produces: {dead:?}"
        );

        for (path, command) in &all {
            let about = command.get_about().map(ToString::to_string);
            assert_eq!(
                bundle.message(&about_key(path)),
                about.as_deref(),
                "`{}` about",
                about_key(path)
            );
            let long_about = command.get_long_about().map(ToString::to_string);
            let distinct_long = long_about.is_some() && long_about != about;
            assert_eq!(
                bundle.message(&long_about_key(path)),
                if distinct_long {
                    long_about.as_deref()
                } else {
                    None
                },
                "`{}` is present exactly when the derive gives a distinct long_about",
                long_about_key(path)
            );

            for arg in command.get_arguments() {
                let id = arg.get_id().as_str();
                let help = arg.get_help().map(ToString::to_string);
                assert_eq!(
                    bundle.message(&arg_key(path, id)),
                    help.as_deref(),
                    "`{}` help",
                    arg_key(path, id)
                );
                let long_help = arg.get_long_help().map(ToString::to_string);
                let distinct_long = long_help.is_some() && long_help != help;
                assert_eq!(
                    bundle.message(&arg_long_help_key(path, id)),
                    if distinct_long {
                        long_help.as_deref()
                    } else {
                        None
                    },
                    "`{}` is present exactly when the derive gives a distinct long_help",
                    arg_long_help_key(path, id)
                );
            }
        }
    }

    /// The consequence of the invariant above, observed on the rendered output: under the
    /// source locale, `--help` of every command is byte-identical to the un-localized one.
    #[test]
    fn localizing_under_the_source_locale_leaves_every_help_page_byte_identical() {
        let plain = rendered_help(&Cli::command());
        let localized = rendered_help(&localize(Cli::command(), &Bundle::for_locale("en")));
        assert_eq!(plain, localized);
        assert!(plain.iter().any(|page| page.contains("--library")));
    }

    #[test]
    fn a_catalog_message_replaces_the_derive_text_at_its_position_only() {
        let localized = localize_with(Cli::command(), &|key| {
            (key == about_key("import")).then(|| "XX".to_owned())
        });
        let import = localized
            .find_subcommand("import")
            .expect("`import` is a subcommand");
        assert_eq!(
            import.get_about().map(ToString::to_string).as_deref(),
            Some("XX")
        );
        // The lockstep rule: a one-paragraph derive comment sets both forms, so both move.
        assert_eq!(
            import.get_long_about().map(ToString::to_string),
            Cli::command()
                .find_subcommand("import")
                .expect("`import` is a subcommand")
                .get_long_about()
                .map(|_| "XX".to_owned())
        );
        let push = localized
            .find_subcommand("push")
            .expect("`push` is a subcommand");
        assert_eq!(
            push.get_about().map(ToString::to_string),
            Cli::command()
                .find_subcommand("push")
                .expect("`push` is a subcommand")
                .get_about()
                .map(ToString::to_string)
        );
    }

    #[test]
    fn an_argument_message_reaches_the_rendered_help_of_its_command() {
        let key = arg_key("import", "library");
        let mut localized = localize_with(Cli::command(), &|k| {
            (k == key).then(|| "YY the library".to_owned())
        });
        let page = localized
            .find_subcommand_mut("import")
            .expect("`import` is a subcommand")
            .render_long_help()
            .to_string();
        assert!(page.contains("YY the library"), "{page}");
        assert!(!page.contains("Path to the Capsule library"), "{page}");
        // Another command's identical-looking argument is a different key, so it is untouched.
        let cull = localized
            .find_subcommand_mut("cull")
            .expect("`cull` is a subcommand")
            .render_long_help()
            .to_string();
        assert!(cull.contains("Path to the Capsule library"), "{cull}");
    }

    #[test]
    fn a_nested_subcommand_is_keyed_by_its_dotted_path() {
        let key = about_key("library.init");
        let localized = localize_with(Cli::command(), &|k| (k == key).then(|| "ZZ".to_owned()));
        let init = localized
            .find_subcommand("library")
            .and_then(|library| library.find_subcommand("init"))
            .expect("`library init` is a subcommand");
        assert_eq!(
            init.get_about().map(ToString::to_string).as_deref(),
            Some("ZZ")
        );
    }

    #[test]
    fn a_missing_key_leaves_the_derive_text_in_place() {
        let plain = rendered_help(&Cli::command());
        let untouched = rendered_help(&localize_with(Cli::command(), &|_| None));
        assert_eq!(plain, untouched);
        // A locale with no catalog of its own falls back to the source entries, which the
        // invariant test proves are the derive text — so an unknown locale is the identity too.
        let unknown = rendered_help(&localize(Cli::command(), &Bundle::for_locale("xx-XX")));
        assert_eq!(plain, unknown);
    }

    #[test]
    fn the_key_grammar_spells_root_and_nested_paths() {
        assert_eq!(about_key(ROOT_PATH), "cli.help.root.about");
        assert_eq!(long_about_key(ROOT_PATH), "cli.help.root.long_about");
        assert_eq!(child_path(ROOT_PATH, "library"), "library");
        assert_eq!(child_path("library", "init"), "library.init");
        assert_eq!(
            arg_key("library.init", "path"),
            "cli.help.library.init.arg.path"
        );
        assert_eq!(
            arg_long_help_key("import", "paths"),
            "cli.help.import.arg.paths.long_help"
        );
    }
}
