---
title: CLI
description: What the capsule command line is for, how to install it, and where its contract lives
status: draft
---

`capsule` is the command line for a Capsule library. It is the only client that performs the
whole data plane end to end — it scans and imports files, seals them locally, opens upload
sessions, drains the sync feed, and rebuilds an index from what is on disk — which is why the
examples in this documentation are commands rather than HTTP requests. The server never sees a
key, so a request transcript would show ciphertext going in and ciphertext coming out; a
transcript of `capsule` shows the operation.

This page is the hand-written half of the CLI reference. [Commands](/reference/cli/commands/) is
the generated half: every command, argument, and option, emitted from the same `clap` definitions
the binary parses with.

## Install

Release builds are published as an archive per target on the
[releases page](https://github.com/justin13888/Capsule/releases), each carrying a single
`capsule` executable. From a checkout, `cargo run -p capsule-cli --` runs the same binary
against the working tree.

## The two things it holds

A `capsule` invocation reads at most two pieces of durable state, and it helps to know which:

- **A library** — a directory named by `--library`, holding the encrypted assets, their sidecar
  metadata, and a SQLite index that can be rebuilt from the sidecars alone
  (`capsule library rebuild`). Every offline command operates on one.
- **A session** — the token pair `capsule auth login` persists, owner-readable, under the
  user's configuration directory. Every networked command reads it, and `capsule reset` removes
  it.

A library is opened with a passphrase. Each command that opens one accepts
`--passphrase-stdin`, so nothing in this reference requires a terminal.

## Where the contract lives

- The behaviour of the import pipeline is [Import Pipeline](/design/import/pipeline/); what
  `capsule push` speaks is the [Upload Protocol](/design/import/upload-protocol/), and what
  `capsule sync` drains is [Download & Sync](/design/import/download-sync/).
- The server endpoints behind the networked commands are the
  [REST API](/reference/api/) reference.
- Terminal output is localized through the catalogs described in
  [Internationalization](/design/i18n/). Help text is not yet: the command tree this reference
  is generated from is English, deliberately and by pinning, so the artifact cannot vary with
  the machine that emits it.

## How the generated page stays true

`capsule-cli` emits `capsule-cli/cli-surface.json` — a description of the command tree, read
straight from the `clap` definitions — and `mise run cli-surface-check` fails the Rust gate if
the committed copy disagrees with the code. The documentation build reads that file and
nothing else; it never runs cargo. So a new option cannot reach users without either appearing
on the generated page or failing CI.

A generated page is never edited. If something on it is wrong, the annotation it came from is
wrong: fix the `clap` `about` or doc comment, run `mise run cli-surface`, and commit the
artifact. The pipeline and the reasoning behind it are [Developer
Documentation](/design/developer-docs/).
