---
title: Migrating from Google Photos
description: Export a Google Takeout archive and bring your originals into a local Capsule library
status: draft
---

Move your library from Google Photos into a local Capsule library: export with Google Takeout,
import with the `capsule` CLI, and verify. No server is needed. The importer's contract is
[Import — Third-Party Importers](/design/import/pipeline/#third-party-importers).

## Before you start

Treat this as copy-and-verify:

- **Delete nothing from Google until the copy is verified.**
- **Keep the Takeout archive** until you trust the result; it is your reference.
- **Run both for a while** before relying on Capsule alone.
- **Back up the Capsule library** itself.

You need free disk for roughly twice the export size (extracted archive plus library) and the
`capsule` CLI.

## Step 1 — Request your export from Google Takeout

1. Open [Google Takeout](https://takeout.google.com/), choose **Deselect all**, then select
   **Google Photos** only.
2. Choose `.zip` and a per-file size. Large libraries are split into **multiple parts**.
3. Click **Create export** and wait for the emails (hours to days).

## Step 2 — Download and extract every part

Download and extract **every** part; a media file and its JSON sidecar can land in different
parts.

```sh
mkdir takeout-extracted
unzip 'takeout-*.zip' -d takeout-extracted
```

Do not rename or reorganize anything: the importer relies on Google's layout.

## Step 3 — Import your originals into a Capsule library

```sh
capsule import ./takeout-extracted --provider takeout --library ~/capsule-library
```

If the parts are in separate folders, name **all of them in one command** so sidecars pair with
their media:

```sh
capsule import ./part-001 ./part-002 ./part-003 --provider takeout --library ~/capsule-library
```

You are prompted for the library passphrase (a new path is initialized with it). The command
scans, reports what is new, duplicate or unsupported, then copies each original in, verifying its
hash.

- Without `--provider takeout`, the Takeout JSON is ignored: originals import, but albums,
  favorites, captions and JSON-only times and locations do not.
- Do not use `--move` for a migration; copying keeps your fallback intact.
- Re-running is **idempotent**: existing assets are skipped by content hash. Re-run to resume an
  interrupted import.

## How Capsule reads a Takeout export

| From Takeout | Becomes |
| --- | --- |
| Capture time and GPS | The file's own EXIF wins; the JSON fills gaps. Google's `(0, 0)` means no location. |
| Description | Caption |
| Favorite | 5-star rating (Capsule has no separate favorite flag) |
| Album | A user tag with the album title; assets land in the library's default album |

A location taken from the JSON is recorded as **manual**, not EXIF, because it did not come from
the file's bytes.

The importer also handles Takeout's quirks: truncated sidecar names, `photo(1).jpg` /
`photo.jpg(1).json` duplicates (kept as distinct assets), `-edited` copies (stacked with their
original, including localized suffixes), and sidecars split across parts. Results are
deterministic across runs.

## What the importer cannot guarantee

The importer is built against the Takeout layout described above. These are its known limits;
they do not fail the import, which is why the verification below is not optional:

- **Only the fields in the table are read.** Every other field in a sidecar (people tags, for
  example) is not imported.
- **A sidecar that cannot be paired or parsed is skipped quietly.** Its media still imports, but
  without that JSON's caption, favorite or fallback time and location. Pairing tries the
  exact name, then the JSON's recorded title, then a shared name prefix of at least 16 characters;
  a renamed file or a shorter truncated name can leave its sidecar unpaired.
- **Edited copies are recognized by a fixed set of localized suffixes** (`-edited`, `-bearbeitet`,
  `-modifié` and a few others). An edited copy in a language outside that set imports as its own
  asset instead of stacking with its original.
- **Google's export is the ceiling.** Capsule imports what Takeout put in the archive; it cannot
  detect an item Google left out. Compare against Google's own item count.
- **A format Capsule cannot decode still imports.** The original is stored byte for byte; only its
  thumbnail or preview may be missing (HEIC decoding is not built in, for example).
- **Google can change Takeout's layout.** A change in how it names or arranges files can degrade
  pairing without an error. Run the checklist below on every migration, not just the first.

## Verify everything end to end

**Counts.** Re-run the same import; a complete import reports `Nothing to import`. Compare against
Google's item count, expecting fewer assets in Capsule because edited copies are stacked.

**Spot hashes.** Capsule imports bytes unchanged, so a source file's SHA-256 is the asset's hash.
Hash a sample of originals from the archive and keep the list:

```sh
find takeout-extracted -type f \( -iname '*.jpg' -o -iname '*.heic' \) -print0 \
  | head -z -n 20 | xargs -0 shasum -a 256
```

(The `-print0` form matters: the `Google Photos` directory name contains a space.)

**Metadata.** Pick a few photos that are in albums, favorited or captioned in Google. Read each
back by hash prefix (eight or more hex characters):

```sh
capsule show --library ~/capsule-library 3b2f9c1e
```

Check that the caption matches, a favorite reads `5/5`, each album is a user tag, and a
JSON-sourced location says `manual`. Missing fields print `(unset)`. `capsule match <file>`
shows what Capsule reads from a source file.

## After you've verified

Once counts, hashes and samples check out and you have used both side by side, you can wind down
Google Photos. Keep the Takeout archive and a backup of your library so you can replay the import.
