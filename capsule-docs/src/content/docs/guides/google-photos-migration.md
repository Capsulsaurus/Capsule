---
title: Migrating from Google Photos
description: Export a Google Takeout archive and bring your originals into a local Capsule library
status: draft
---

This guide walks you through moving your library out of Google Photos and into
Capsule: requesting a **Google Takeout** export, extracting it, importing your
originals into a local Capsule library, and verifying the result end to end.

It is a practical how-to, not a specification. For the normative contract behind
the importer — the source-adapter seam, the metadata precedence rule, and the
Takeout quirks it reconciles — see
[Import — Third-Party Importers](/design/import/pipeline/#third-party-importers).

## Before you start: migrate carefully

Migrations move irreplaceable data. Treat this as a copy-and-verify operation,
never a move-and-hope one:

- **Do not delete anything from Google until you have verified everything.**
  A migration is done when the copy in Capsule is proven complete and correct —
  not when the import command finishes.
- **Keep the original Takeout archive** (the downloaded `.zip`/`.tgz` parts)
  until you are confident in the result. It is your ground truth for the
  verification steps below.
- **Run both systems in parallel for a while.** Keep your Google Photos account
  active alongside Capsule for a cutover period — long enough to browse, search,
  and spot-check the imported library in real use — before you rely on Capsule
  alone.
- **Follow deployment best practices for whatever holds the result.** If you are
  importing into a [self-hosted](/guides/self-hosting/) instance, back up the
  library and its object store; if you are importing into a local library on a
  laptop, make sure that library is itself backed up.

## What you'll need

- A Google account with photos to export.
- A machine with enough free disk to hold the **extracted** Takeout archive
  (uncompressed) *and* the imported Capsule library — plan for roughly twice the
  reported export size while both copies coexist.
- The `capsule` CLI. This guide uses the local-library import surface described
  in [Getting Started](/guides/getting-started/); no server is required to
  import.

## Step 1 — Request your export from Google Takeout

1. Go to [Google Takeout](https://takeout.google.com/).
2. Choose **Deselect all**, then select **Google Photos** only. Exporting just
   Photos keeps the archive smaller and the extracted layout simpler.
3. (Optional) Use **All photo albums included** to narrow the export to specific
   albums or years. Exporting everything is the simplest path and lets Capsule
   see your full album structure.
4. Click **Next step** and choose the delivery and format options:
   - **Delivery:** "Send download link via email" is fine for most people.
   - **File type:** `.zip` is the easiest to extract on any platform.
   - **File size:** pick a per-file size (2 GB, 10 GB, 50 GB…). Google splits a
     large library into **multiple parts** at this boundary. Capsule handles
     split exports — see [Step 2](#step-2--download-and-extract-every-part).
5. Click **Create export**. Google prepares the archive in the background; large
   libraries can take hours or days. You will receive an email per part when
   they are ready.

## Step 2 — Download and extract every part

Google may hand you several files, for example `takeout-…-001.zip`,
`takeout-…-002.zip`, and so on.

1. **Download every part.** A single media file and its metadata sidecar can
   land in *different* parts — Capsule reunites them, but only if every part is
   present *in the same import run* (see [Step 3](#step-3--import-your-originals-into-a-capsule-library)).
2. **Extract every part.** You can extract them side by side into one parent
   folder, or each into its own folder — both work. For example:

   ```sh
   mkdir takeout-extracted
   unzip 'takeout-*.zip' -d takeout-extracted
   ```

   After extraction you will have one or more `Takeout/Google Photos/…` trees
   containing your media, per-file JSON sidecars, and per-album folders.

Do not rename or reorganize the extracted files. Capsule relies on Google's
on-disk layout — the `Takeout/Google Photos/<album>/` folders, the
`<name>.<ext>.json` (and `…supplemental-metadata.json`) sidecars, and the
per-album `metadata.json` manifests — to reconstruct structure.

## Step 3 — Import your originals into a Capsule library

Point the `capsule import` command at the extracted export with
`--provider takeout`, which reads the tree as a Google Photos export rather than
as a plain folder of files:

```sh
capsule import ./takeout-extracted --provider takeout --library ~/capsule-library
```

If you extracted the parts into separate folders, **name every part in one
command** so a media file and a sidecar that landed in different parts are still
paired:

```sh
capsule import ./part-001 ./part-002 ./part-003 --provider takeout --library ~/capsule-library
```

Importing the parts in separate passes is safe — Capsule deduplicates by content
across runs — but a sidecar whose media file was in another pass has nothing to
pair with, so its metadata is lost for that file. One run over every part is the
reliable way.

Without `--provider takeout` the same command still imports your originals and
deduplicates them, but reads the tree as ordinary files: the Takeout JSON is
ignored, so albums, favorites, captions, and JSON-only times and locations do not
come across.

You will be prompted for the library passphrase (a first import into a fresh
path initializes the library under that passphrase). The command then:

1. **Scans** the tree for supported media and reports how many import candidates
   it found.
2. **Plans** the import against the library, reporting how many assets are new,
   how many are duplicates of assets already present, and how many are
   unsupported or errored.
3. **Executes** the plan, copying each original into the library on the signed
   lifecycle path, hashing it on the way in, and reporting a per-file result.

Useful flags:

- `--move` moves files into the library instead of copying them. **Avoid this
  for a migration** — copying keeps your extracted archive intact as a fallback
  until you have verified the result.
- `--force` re-imports files even if an identical asset already exists. You
  rarely need this; the default deduplication is what makes re-running safe.

Re-running the same import is **idempotent**: assets already present are
recognized by content hash and skipped, so a second pass reports `Nothing to
import`. This is what lets you resume an interrupted import — just run the same
command again.

### What the CLI import applies

With `--provider takeout`, the import applies everything the Takeout **source
adapter** understands (described in [the next
section](#how-capsule-reads-a-takeout-export)) and writes it into each imported
asset's signed metadata record:

- **Capture time and location.** A file's own EXIF wins; where the timestamp or
  GPS fix lived *only* in the Takeout JSON, the exporter's value fills the gap.
- **Captions.** A user-typed description becomes the asset's caption.
- **Favorites.** A starred/favorited photo is imported with the **maximum star
  rating** (5). Capsule has no separate "favorite" flag, so this is where a
  favorite lands; your culling flags are left untouched.
- **Album membership.** Google album titles are preserved as **user tags** on
  each asset in that album — so an album is reconstructible as a search over its
  title. Capsule does not create container albums from an import: an import
  never invents destinations, so every asset lands in the library's default
  album with its Google album recorded as a tag.

Two fidelity notes worth knowing before you reconcile anything:

- A location that came from the Takeout JSON is recorded as **manually
  supplied**, not as EXIF — it was read out of Google's record, not out of your
  file's bytes, and the metadata record is signed, so it says where the value
  actually came from.
- Google exports both a user-editable location and its own copy of the file's
  EXIF location. Capsule folds them into one point, so a fix that came from
  Google's EXIF copy is also recorded as manually supplied.

## How Capsule reads a Takeout export

This section documents what the Takeout adapter understands about an export —
the behavior `--provider takeout` applies, and what the mapping above is built
on.

### The metadata precedence rule

For each media file, Capsule folds two sources of metadata:

- **Embedded EXIF wins** for capture time (`DateTimeOriginal`) and GPS — the
  file's own bytes are authoritative when they carry these.
- **The Takeout JSON wins** for things the file bytes never carried: **album
  membership**, the **favorite/starred** flag, and **user-typed descriptions**.
- When a file has no embedded capture time or GPS, the exporter's
  `photoTakenTime` (falling back to `creationTime`) and `geoData` fill the gap.
  Google's `(0, 0)` "no location" sentinel is treated as absent.

### The Takeout quirks it reconciles

Google's export format has several well-known irregularities. Capsule handles
each so your library reconstructs cleanly:

- **Truncated sidecar names.** Google truncates long sidecar filenames, so a
  media file and its JSON may share only a prefix. The JSON's `title` field
  carries the true original name, and a prefix fallback re-pairs the two.
- **`(1)` duplicates.** A second file with the same name becomes
  `photo(1).jpg`, but its sidecar keeps the counter *after* the extension:
  `photo.jpg(1).json`. Both are normalized so they pair correctly without
  cross-matching the un-suffixed original — the duplicate stays a distinct
  asset.
- **Edited / original pairs.** `photo.jpg` (your original) and
  `photo-edited.jpg` (Google's edit) collapse into **one** stacked asset; the
  edited rendition never becomes a separate item. The localized `-edited` suffix
  (for several common languages) is recognized.
- **Split archives.** When a media file and its sidecar land in different export
  parts, all parts are walked into one pool before pairing, so the pair is
  reunited — which is why [Step 2](#step-2--download-and-extract-every-part)
  insists you download and extract *every* part.

Extraction is **deterministic**: the same export yields the same result on every
run, regardless of filesystem ordering, which is what makes re-running an import
safely skip completed work.

## Verify everything end to end

Before you trust the migration, verify it. Do this while your Google Photos
account and your original Takeout archive are both still available.

### Counts

- After the import, note the command's summary: candidates found, assets
  imported, and duplicates skipped.
- **Re-run the exact same import command.** A correct, complete import reports
  `Nothing to import` (every candidate is recognized as an existing asset). If a
  second run still wants to import files, investigate before deleting anything.
- Sanity-check the magnitude against Google Photos' own item count for the
  library or albums you exported. Expect Capsule's asset count to be *lower* than
  Google's raw file count, because edited/original pairs collapse into a single
  stacked asset.

### Spot hashes

Capsule is content-addressed: every original is hashed on the way in, and the
import **verifies that hash after the copy** — a corrupted transfer is reported
as an error rather than silently stored, and identical content is deduplicated.
You can lean on this for integrity:

- The idempotent re-run above is itself a hash check: it proves every asset in
  the library matches content Capsule already holds.
- For an independent check, hash a sample of source originals in your retained
  Takeout archive before you delete anything from Google, and keep that record:

  ```sh
  find takeout-extracted -type f \( -iname '*.jpg' -o -iname '*.heic' \) -print0 \
    | head -z -n 20 | xargs -0 shasum -a 256
  ```

  The null separators are not decoration: a Takeout export contains a directory
  literally named `Google Photos`, and the whitespace-separated form splits every
  path on that space and hashes nothing.

  Because Capsule imports the original bytes unchanged, these source hashes are
  your reference for confirming the same files are the ones that came across.

### Metadata sampling

Sample a handful of source originals with any EXIF tool and keep the record —
capture time, camera make/model, and GPS baked into the file. `capsule match`
reports what Capsule reads off a **source file** (its hash, size, and
timestamps), which is how you confirm a specific file is the one that came
across:

```sh
capsule match './takeout-extracted/Takeout/Google Photos/Photos from 2021/example.jpg'
```

Do the same for the **exporter-supplied** metadata, since that is what
`--provider takeout` adds: pick a few photos you know are in an album, are
favorited, or carry a typed caption in Google Photos, and note them down.

Then read each one back out of the library. `capsule show` prints what the
imported asset's **signed sidecar** records, and it takes the SHA-256 you already
have from the spot-hash step — Capsule imports bytes unchanged, so the source
file's hash is the asset's hash. A prefix of eight or more hex characters is
enough; if it happens to match more than one asset, the command refuses and asks
for more of the hash rather than guessing:

```sh
capsule show --library ./my-library 3b2f9c1e
```

```text
Asset 019a2d3c-…
  Album:           …
  Content type:    image/jpeg
  SHA-256:         3b2f9c1e…
  Dimensions:      4032×3024
  Captured:        2021-07-04T18:22:09Z
  Imported:        2026-09-02T10:15:42Z
  Caption:         Grandma's 80th
  Rating:          5/5
  User tags:       Family reunion 2021
  AI tags:         (unset)
  GPS:             37.774900, -122.419400 (WGS-84, manual)
  Cull flag:       neutral
  …
```

Check the rows against your notes using the mapping table above: the caption is
the Google description, a favorite reads as `5/5`, each album the photo was in is
a user tag, and a GPS fix that came from the Takeout JSON rather than the file's
own EXIF is marked `manual` (a fix is always printed with its datum, so a GCJ-02
coordinate stored verbatim is never mistaken for WGS-84). A field the export did not carry prints as
`(unset)` rather than being omitted, so a missing caption is something you can
see. A mapping you disagree with is worth catching here: the values live in a
signed sidecar, and changing one later is a signed metadata update per asset.

Retain the Takeout archive and keep Google Photos alive through the cutover all
the same — the sample tells you the mapping is right, not that every one of tens
of thousands of assets is.

## After you've verified

Only once counts reconcile, spot hashes match, and you have sampled metadata to
your satisfaction — and after you have run both systems in parallel long enough
to trust the result — should you consider winding down Google Photos. Even then,
keep the Takeout archive and a backup of your Capsule library: the archive is
what lets you replay the import if anything about the result ever surprises you.
