---
title: CLI
description: What the capsule command line is for, how to install it, and where its contract lives
status: draft
---

`capsule` is the command line for a Capsule library. It is the only client that performs the whole
data plane end to end: it scans and imports files, seals them locally, opens upload sessions,
drains the sync feed and rebuilds an index from what is on disk. That is why the examples in this
documentation are commands rather than HTTP requests. The server never sees a key, so a request
transcript would show ciphertext going in and coming out; a transcript of `capsule` shows the
operation.

[](/reference/cli/commands/) lists every command, argument and option, generated from the `clap`
definitions the binary parses with and verified in CI.

## Install

No release has been published yet. From a checkout, `cargo run -p capsule-cli --` runs the binary
against the working tree and `cargo install --path capsule-cli` installs it.

## The two things it holds

A `capsule` invocation reads at most two pieces of durable state.

- **A library:** a directory named by `--library`, holding the encrypted assets, their sidecar
  metadata, and a SQLite index that can be rebuilt from the sidecars alone
  (`capsule library rebuild`). Every offline command operates on one.
- **A session:** the token pair `capsule auth login` persists, owner-readable, under the user's
  configuration directory. Every networked command reads it, and `capsule reset` removes it.

Eleven commands unseal a library, and all eleven take its passphrase: `capsule import`,
`capsule push`, `capsule restore`, `capsule cull`, `capsule show`, `capsule library list`,
`capsule repair capture-time`, `capsule label`, `capsule region`, `capsule edit` and
`capsule sync reannounce`. `capsule sync --library` runs the last itself when the server has
rebuilt the library's index. Each accepts `--passphrase-stdin` as well as prompting, so each runs
unattended. The other `capsule library` subcommands, `info` and `rebuild`, read only the version
file, the sidecars and the index, none of which is sealed, so they need no passphrase and offer no
flag for one.

## Bare `capsule`

- **Terminal or not.** With no command while stdin and stdout are both terminals, `capsule` opens a
  full-screen TUI; `q` or Ctrl-C leaves it and `?` lists its keys. Anywhere else (a pipe, a
  redirect, a CI runner) bare `capsule` prints the help on stderr and exits 2, as a missing command
  always has, so a script never blocks on a screen.
- **Logging.** While the TUI holds the terminal, log output goes to `tui.log` in the CLI's cache
  directory instead of stderr, replaced each time the TUI opens. A password, passphrase or
  authenticator code typed into the TUI is masked on screen, and neither it nor an issued share URL
  is written to that log.
- **Confirmation.** Signing out and revoking a share link ask `y` or `n` first in the TUI. The
  subcommands do not ask, so scripts keep working.
- **Subcommand twins.** Every flow the TUI offers stays reachable as a subcommand, with `--json`
  where it reports. The library browser's twins are
  `capsule library list --library PATH [--view timeline|albums|stacks]`, which lists the same
  timeline, albums and stacks from the same snapshot, and `capsule show ASSET --library PATH`,
  whose `--json` document carries the asset's provenance history record by record.
- **Previews.** The library browser asks the terminal once, at start-up, whether it draws Kitty,
  Sixel or iTerm2 graphics. Where it draws none, the preview pane says so in text and no original
  is decoded for it. Regions are drawn over the preview in half blocks, which every colour terminal
  shows.
- **Editing.** From the browser, the label editor (`e`) and the time-and-place screen (`m`) write
  to the open library with the passphrase it was opened with, kept in memory only while it stays
  open. Each edit unlocks the library, writes and closes it again, through the same code as
  `capsule label`, `capsule region` and `capsule edit`.

## Where the contract lives

- The import pipeline's behaviour is [](/design/import/pipeline/). `capsule push` speaks
  [](/design/transfer/upload/) and `capsule sync` drains [](/design/sync/).
- `capsule restore` fetches originals over [](/design/transfer/download/) and opens them with the
  library's album keys. It writes an original only once its ciphertext matches the signed manifest
  and its plaintext matches the signed metadata's digest. An interrupted run resumes from the part
  files it left in `OUT/.capsule-restore/`.
- The server endpoints behind the networked commands are the [REST API](/reference/api/)
  reference.
- The CLI and TUI are English-only. The shared `error.*` codes render through the English catalog
  described in [](/design/i18n/), and the remaining locale negotiation is to be dropped. The command
  tree this reference is generated from is English, so the artifact cannot vary with the machine
  that emits it.
