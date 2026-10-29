---
title: User Guide
description: Day-to-day use of a Capsule library with the capsule CLI — import, back up, verify, organize, and delete safely.
status: draft
---

This guide covers the `capsule` CLI, the reference client. Commands marked *(planned)* are on
the [roadmap](https://github.com/Capsulsaurus/Capsule/blob/master/ROADMAP.md) and not yet
available. `capsule <command> --help` is authoritative for flags; the
[CLI reference](/reference/cli/) lists them all.

:::caution
Capsule is alpha. Until the first dogfood release, keep an independent backup of any original
you import.
:::

## Set up

1. Create a library: `capsule library init --name "Photos"`. The library is a local, encrypted
   directory; its passphrase protects the keys on this machine.
2. Sign in to your server: `capsule auth register` or `capsule auth login` (`--oidc` or
   `--device` for single sign-on).
3. Set up recovery: `capsule auth escrow`. Store the printed recovery secret offline. Without it
   and your passphrase, a lost machine means a lost library — the server cannot decrypt anything.

## Import and back up

- `capsule import <path>` copies files into the library, signs each asset's provenance and
  metadata, and generates thumbnails. `--provider takeout` imports a Google Takeout archive
  with its sidecar metadata ([migration guide](/guides/google-photos-migration/)).
- `capsule push` uploads the library. Uploads resume where they stopped; re-running `push` is
  always safe. After each asset, `push` asks the server to confirm it durably holds every blob
  and keeps the server's signed custody receipts in the library. An asset that fails is
  reported and the run carries on; the command exits non-zero if any asset failed, and
  `--json` lists each asset's outcome.
- `capsule import --push` does both in one step.
- **Keep your originals until custody is verified.** An asset is safely stored only once the
  server reports it durable and has issued a signed custody receipt — what `push` reports as
  verified. Do not delete source files before then. `import --move --push` deletes each
  source only after its asset is verified; without `--push`, `--move` keeps every source and
  says so, and a later `import --move --push` over the same files releases them.
- `capsule restore` *(planned)* downloads originals and checks them byte for byte against what
  was signed at import.

## Organize

- Labels: free-form labels on an asset, a stack (RAW+JPEG pair, burst, bracket), an album, or a
  region of an image. Labels you add are kept separate from AI-suggested ones; you can promote
  or dismiss a suggestion. *(Asset labels exist today; stack, album and region labels are
  planned.)*
- `capsule show <asset>` prints what an asset's signed metadata records: caption, rating,
  labels, location, capture time.
- `capsule repair capture-time` finds assets whose capture time disagrees with the original's
  EXIF and, with `--apply`, corrects them. Location editing is *(planned)*.

Every edit is a signed, append-only change: the earlier value stays in the asset's provenance
history, and concurrent edits from two devices merge deterministically.

## Delete safely

Before deleting assets, it is recommended to fix all metadata such as geolocation while you have
the necessary context from surrounding assets. A photo with no location of its own is easy to
place while its neighbours from the same walk are still in the library; once they are gone, so
is that context.

- `capsule cull` flags assets as pick, neutral or reject and filters by flag.
- `capsule cull --sweep` moves rejects to the trash. Trashed assets stay recoverable for the
  retention window (`--retain-days`) before they are purged.
- `capsule trash delete --asset <id>` moves an asset to the trash on the server as well as in
  the library, and `capsule trash restore --asset <id>` brings it back. An asset already in
  the trash, for example after `capsule cull --sweep`, keeps the retention window it was
  trashed with: `--retain-days` applies only to a fresh delete, and `--json` reports the
  window sent as `retention_until`. A restore after the
  retention window has closed is refused (`retention_elapsed` under `--json`) and is not
  retried: the asset stays in the trash until it is purged.

## Share

- `capsule share create --album <id>` or `--asset <id>` issues a view-only link; `--protect`
  adds a link passphrase and `--expires-at` an expiry. The link carries its key in the URL
  fragment, which never reaches the server.
- `capsule share revoke <id>` stops a link from opening.

## Sync another machine

- `capsule sync` pulls the library's change feed from the server; `capsule list` shows what it
  delivered.
- `capsule auth recover` brings the account onto a new machine from the recovery secret.
