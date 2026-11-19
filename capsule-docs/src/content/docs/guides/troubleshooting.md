---
title: Troubleshooting
description: Collect the version and logs a report needs, and fix the common problems with a Capsule server, an import or a library.
status: draft
---

Start here when something goes wrong. If nothing below fixes it, [](/support/) says where to ask,
and the diagnostics in the first section are what a bug report needs.

## Collect diagnostics

### Version

No release has been published, so the version number does not tell builds apart. Everything is
built from source, so report the commit you built from: `git rev-parse --short HEAD` in your
checkout. The CLI has no version flag, and `capsule-server --version` prints only the crate version,
which every build shares.

### Logs

The CLI and the server write logs to **stderr**. Stdout carries only what a command prints, and
under `--json` that is a single JSON document, so you can capture each one separately:

```sh
RUST_LOG=debug capsule push --library ~/Photos.capsule 2> capsule.log
```

- `RUST_LOG` sets how much is logged. A release build logs at `info` by default and a debug build
  at `debug`. Use `RUST_LOG=debug` when you reproduce a problem, and `RUST_LOG=trace` only when
  asked: it is very large.
- The server's log format is set by `LOG_FORMAT`: `pretty` by default in debug builds, `json` in
  release builds ([](/development/local-development/#logs)).
- The full-screen TUI (a bare `capsule` in a terminal) cannot log to the screen it draws on, so it
  writes `tui.log` to the CLI's cache directory, which `capsule status` prints as its Cache
  Directory. The file is overwritten each session, so copy it before you start the TUI again.
- `--json` output is the most precise record of what a command did, because it reports each asset's
  outcome. Attach it when a command reports failures.

**Redact before you share.** Remove passphrases, recovery secrets, tokens, keys, server addresses
and personal file paths. Never paste a library passphrase or a recovery secret anywhere, including
a private report.

## The server

### It refuses to start

- **Exit status 2, naming variables.** A durable server needs `DATABASE_URL`, `BLOB_ROOT`,
  `JWT_ED25519_DER` and `ATTESTATION_KEY_SEED`, and names every missing one at once. See
  [](/guides/self-hosting/#configure).
- **The database schema is older than the binary.** `serve` never migrates. Run
  `capsule-server-migration up` on every upgrade, before `serve`
  ([](/guides/self-hosting/#migrate-then-start)).
- **A trusted-proxy setting is refused.** `TRUSTED_PROXIES` refuses a `/0` network, and
  `TRUSTED_PROXY_HEADER` accepts only `x-forwarded-for` or `forwarded`.

A server that is running prints `listening on <url>` to stdout.

### Clients can't sign in or connect

- `capsule server-info` shows how the server signs people in: whether it has an identity provider
  and whether it refuses passwords. A password sign-in to a server with passwords disabled fails by
  design.
- Behind a reverse proxy, every client shares the proxy's rate limit until the proxy is named in
  `TRUSTED_PROXIES` or `TRUSTED_PROXY_HOPS`. When several people are rate limited at once, check
  this first ([](/guides/self-hosting/#configure)).

### The index and the blob store disagree

`capsule-server scrub` is a read-only integrity check, and it exits non-zero on any finding. After
you restore a backup or lose the database, `capsule-server rebuild` compares the index with the
owner ledgers. It changes nothing until you stop `serve` and run `capsule-server rebuild --apply`
([](/guides/self-hosting/#maintenance)).

## Imports and uploads

- **A `push` stopped partway.** Run it again. Uploads resume where they stopped, so re-running is
  always safe. `push` exits non-zero when any asset failed, and `--json` lists each asset's outcome
  ([](/guides/user-guide/#import-and-back-up)).
- **Keep your originals until custody is verified.** Don't delete a source file until `push`
  reports its asset verified, or use `import --move --push`, which deletes each source only after
  its asset is verified.
- **A Google Photos export looks incomplete.** Check that every part of the Takeout export was
  downloaded and extracted, and read what the importer cannot guarantee
  ([](/guides/google-photos-migration/#what-the-importer-cannot-guarantee)).
- **Capture times are wrong.** `capsule repair capture-time` reports every asset whose signed
  capture time disagrees with the original's EXIF, and `--apply` corrects them. Each correction is
  a permanent signed record, so run it without `--apply` first.

## Check a library's integrity

If photos look missing or damaged, **stop and don't delete anything** while you find out:
deleting can remove the context you need to recover.

1. `capsule verify --library <path>` asks the server whether it durably holds each original.
   `--deep` has the server re-hash the stored bytes instead of only checking they are present, and
   `--signed` keeps the server's signed verdict in the library.
2. Trashed assets stay recoverable for their retention window:
   `capsule trash restore --library <path> --asset <id>` brings one back
   ([](/guides/user-guide/#delete-safely)).
3. A lost device or key has its own procedures: [](/design/backup-recovery/#recovery-paths).

If data is lost or corrupted, report it as a bug with the output of each step. Losing or
corrupting data is a defect, whatever its cause.

## Still stuck

[](/support/) lists where to ask a question and how to file a bug report that can be acted on.
