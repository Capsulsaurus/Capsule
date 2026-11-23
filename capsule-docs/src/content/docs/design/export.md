---
title: Export
description: Plaintext export of assets and albums to a folder or archives, with its options, archive format, resumability and command-line flow
status: draft
---

**Rule:** an export is the user's own plaintext, written where the user chose, in formats any tool reads. It never writes ciphertext and never changes an asset, its metadata or its provenance.

Three things leave a library, and they must not be confused:

| Thing | Holds | Read by | Owner |
| --- | --- | --- | --- |
| **Export** | Decrypted originals or renditions, optional sidecars, a checksums manifest | Any tool: a file manager, an unzip tool, another photo app | This doc |
| **Backup artifact** | Ciphertext, wrapped keys, provenance; signed | Capsule restore only | [](/design/backup-recovery/#backup-artifact) |
| **Delegation bundle** | One upload's ciphertext and signed manifest | The server, carried by another device | [](/design/peering/#delegation) |

An export is not a backup: it carries no keys, no provenance and no signatures, and Capsule does not restore from it. An `original` export with XMP sidecars re-imports through the ordinary import pipeline ([](/design/import/pipeline/)) as new assets.

## Trust Boundary

- **Export is a boundary crossing** under [](/design/metadata/#privacy-on-export), which owns the strip rules. Every row is chosen for that one export and never stored, and the default is strict. Two presets cover the common cases without making either sticky: **share** (the strict default) and **archive** (every row kept, for a user moving their own library). The report states which rows were stripped.
- **Decryption is on the device.** The server sees only the ciphertext fetches an export makes, which are the same reads a viewer makes.
- **Every byte is verified before it is final.** An original's plaintext is checked against the signed sidecar digest ([](/design/metadata/#sidecar-schema-v1)) while it is written. A remote original is fetched and verified against its content address first ([](/design/transfer/download/#resumption-and-verification)). An entry that fails is never left looking complete ([](#resumable-export)).
- **Renditions decode remote-origin bytes** only where [](/design/clients/#sandboxed-decoder) allows, through `capsule-core::media` ([](/design/thumbnails/#media-is-rawshifts)).
- **The export never writes outside the sink.** Entry names are generated, never taken verbatim from metadata ([](#file-and-folder-names)), so no name can traverse or escape.

## Architecture

One Rust module, `capsule-library::export`, owns planning, rendering, naming, sidecars, archive writing, the manifest, resume and progress. It is exposed unchanged over UniFFI to the apps and over wasm to the web client, and the CLI calls it directly. No client re-implements any of it.

- **Native code supplies only a sink**: a sequential byte stream of named entries. A sink creates an entry, writes bytes, finishes or aborts it, and says whether it can reopen a finished file at an offset. The core never seeks the sink.

| Client | Sink |
| --- | --- |
| CLI, desktop | A directory or file on the local filesystem (reopenable) |
| iOS, iPadOS, macOS | A destination the user picked in the system document picker |
| Android | A Storage Access Framework tree or document the user picked |
| Web | A user-picked directory or file where the browser offers one, otherwise a streamed download per archive part |

- **Plan, then run.** Planning is pure over the library and the options. It fixes the entry list, every name, each entry's part and the totals, and reports refusals and warnings, before any byte is written. A run executes a plan. A dry run is a plan with no run.
- **The web client exports too.** An export is a read, which the read-only web client is allowed ([](/design/clients/#web-client)).

## Options

The option set is a superset of what Apple Photos and Google Takeout offer, with defaults that suit a user who did not open the options.

| Option | Values | Default | Apple Photos | Google Takeout / Drive |
| --- | --- | --- | --- | --- |
| Scope | assets, albums, a smart album or search result, a date range, the whole library | the selection | selection | per product |
| Version | `original` (byte-identical original) or `current` (the asset as the library shows it now) | `current` | Export Unmodified Original or Export N Photos | originals, plus `-edited` copies |
| Kind (`current` only) | `original`, `jpeg`, `png`, `tiff`, `jxl` | `original` | JPEG, HEIC, TIFF, PNG | — |
| Quality (JPEG) | low, medium, high, maximum | high | Low to Maximum | — |
| Size (`current` only) | full; small, medium or large (640, 1280, 2048 px long edge); custom width, height or long edge | full | Small, Medium, Large, Full Size, Custom | — |
| Colour profile | keep, sRGB | keep | Colour Profile | — |
| Video | `original` | `original` | 480p to 4K | originals |
| Paired files | together, still only | together | originals: both files; renditions: still only | both files |
| Stacks | all members, primary only | all members | — | — |
| Hidden assets | excluded, included | excluded | — | — |
| File names | `original`, `title`, `sequential` (with prefix), `id` | `original` | Use File Name, Use Title, Sequential | original name |
| Subfolders | `none`, `album`, `date` (`YYYY/MM` or `YYYY-MM-DD`), `album/date` | `none` | None, by day or moment | album folders, `Photos from YYYY` |
| Sidecars | none, XMP, JSON, both | none | Export IPTC as XMP | JSON beside each file |
| Metadata fields | title and caption, keywords (labels), rating, people (regions) | title, caption, keywords, rating | Title, Caption, Keywords | in the JSON |
| Location | strip, coarse (stated precision), full | coarse | Location Information on or off | kept |
| Container | folder, `zip`, `tar`, `tgz` | `zip` for more than one file | folder | `zip` or `tgz` |
| Split size (archives) | none, 2, 4, 10, 50 GB | 4 GB | — | 1 to 50 GB parts |
| Checksums manifest | always | always | — | — |

- **Version.** `original` writes the imported bytes unchanged, with their embedded metadata as imported. `current` applies the library's current state: the user's capture-time and location corrections, caption, labels and rating go into the sidecars, and a rendition (another kind or size) is encoded from the original. Capsule has no pixel edits today. When it gains them, `current` renders them and `original` stays byte-identical.
- **Embedded metadata.** `original` cannot strip what the file itself carries, such as EXIF GPS or a camera serial, because it is byte-identical. The plan counts the files that embed location and the run warns before it starts. Stripping embedded metadata needs `current` with a rendition kind, whose output carries only the metadata the export writes. Rewriting an original's embedded metadata in place waits on Rawshift metadata writing.
- **Kinds Capsule cannot produce.** A kind or size the build has no encoder for is refused at plan time, never silently replaced by another. HEIC output is not offered, because its encoders are excluded by [](/design/licensing/). Video renditions wait on the video transcoder ([](/design/licensing/#the-video-transcoder-seam)), so video exports as the original.
- **Paired files and stacks** follow [](/design/organization/#asset-stacking). A Live Photo's still and clip, and a RAW+JPEG pair, export as one **file group**. Its members share a name stem, land in the same folder and the same archive part, and are never split. "Still only" drops the clip; "primary only" exports each stack's primary.
- **Location.** Coarse is the precision [](/design/metadata/#privacy-on-export) defines, which MUST be stated in metres and uniform at every latitude, with the user's exclusion zones applied on export exactly as on share. Strip removes location from the sidecars and, for renditions, from the file.
- **Hidden and trashed.** Hidden assets ([](/design/organization/#hidden-assets)) are excluded unless the user includes them for this export. Trashed assets are never exported.

## File and Folder Names

Names are generated from the scheme, then made safe for the strictest common target. The same plan always yields the same names.

- **Schemes.** `original` is the file name the asset was imported with; an asset with none recorded falls back to its capture time, `YYYY-MM-DD_HHMMSS`. `title` is the first line of the caption, falling back to `original`. `sequential` is the prefix plus a zero-padded counter in timeline order, padded to the width of the total. `id` is the asset id. The extension always comes from the written content type, never from the source name.
- **Sanitized.** Names are Unicode NFC and stored as UTF-8 (ZIP bit 11). Every character that Windows, macOS or Android storage refuses (`<>:"/\|?*`, control characters) becomes `_`. Trailing dots and spaces, and Windows reserved device names, are escaped. A component is cut to 255 bytes at a character boundary, keeping the extension. No name is absolute, contains `..` or is a link.
- **Collisions** are resolved case-insensitively, because the destination may fold case. The first entry in plan order keeps the name, and later ones gain a space and `(2)`, `(3)`, and so on before the extension. A file group shares its disambiguator.
- **Folders** come from the subfolder scheme: an album name sanitized like a file name, or a date from the effective capture time in the asset's own offset. An asset in several albums under `album` is written once per album, and the manifest records the copies.
- **Times.** Each file's modification time is set to its effective capture time. In a ZIP it is the extended-timestamp field in UTC, and the DOS field carries the same instant.

## Sidecars

- **XMP** is written per file as `{stem}.xmp`, the name Apple Photos, Lightroom and most tools read. Where two files in one folder share a stem (a RAW+JPEG pair), both use `{name}.{ext}.xmp`. It carries the caption (`dc:description`), the first caption line as title (`dc:title`), labels (`dc:subject`), rating (`xmp:Rating`), the effective capture time (`exif:DateTimeOriginal`, `photoshop:DateCreated`), location after the location option, and, when people are kept, the regions as Metadata Working Group regions with their labels.
- **JSON** is written per file as `{name}.{ext}.json`, a documented `capsule-export-sidecar/v1` object. It holds the asset id, the as-imported and effective capture times, caption, labels, rating, location with its precision in metres or none, album names, stack id, type and role, regions when kept, and the plaintext digest.
- Both are produced from the stripped copy of the sidecar ([](/design/metadata/#privacy-on-export)), never from the library's own.

## Archive Format

The archive library is the `zip` crate (zip2) for writing and `rc-zip` for reading ([ADR-0018](https://github.com/Capsulsaurus/Capsule/blob/master/adr/0018-export-archives-are-written-with-zip-and-read-with-rc-zip.md)).

- **ZIP** per PKWARE APPNOTE 6.3.10, written front to back with no seek:
  - Media entries are **stored** (method 0), since media is already compressed. Sidecars and the manifest MAY be deflated.
  - Every entry sets general-purpose bit 3 and is followed by a **data descriptor**, because its CRC-32 is known only after its bytes stream through.
  - An entry whose declared size is `0xFFFFFFFF` bytes (4 GiB − 1) or more uses **ZIP64**: a ZIP64 extra field in its local header and 8-byte sizes in its descriptor. A part with 65,535 entries or more, or with any offset of `0xFFFFFFFF` or more, ends with the ZIP64 end-of-central-directory record and locator. Each limit is the field's all-ones value, which APPNOTE reserves to mean "see the ZIP64 record", so reaching it, not passing it, requires ZIP64.
  - No encryption, no symlinks, no executable modes: every entry is a regular file.
- **`tar`** is POSIX pax, uncompressed. **`tgz`** is the same, gzipped, offered for parity with Takeout. Gzip saves almost nothing on media, so `zip` is the default.
- **Parts.** With a split size, each part is a complete, independently readable archive with its own central directory. It is never a spanned or split ZIP, which most tools cannot open. Parts are named `{name}-001.zip`, `{name}-002.zip`, and so on.
  - Sizes are decimal (4 GB is 4,000,000,000 bytes), so a 4 GB part fits a FAT32 volume.
  - A part closes before an entry or file group that would take it past the size. A single file group larger than the split size gets a part of its own, which the plan reports as oversize.
  - Every part, including an unsplit export, also closes at **250,000 entries**. This bounds the central directory held in memory, so an export with more entries is always multi-part.
- **Deterministic.** The same plan writes the same entry order, names and bytes. Only the manifest's export time differs between runs.

## Manifest and Checksums

Every export carries its own integrity record.

- **`SHA256SUMS`** in GNU coreutils format, one line per entry in that part (or folder) with its relative path, so `sha256sum -c SHA256SUMS` checks it with no Capsule installed.
- **`capsule-export.json`**: the format version, the options, the export time, the part index and count, and per entry its path, asset id, role in its file group, size, SHA-256, the provenance head it was written from, and what the strip removed.
- Both are the last entries of each part. In a folder export they are at the root, written last through a temporary name and a rename, so a manifest present means the export it describes is complete.
- For an `original`, the SHA-256 is of the imported bytes. Those bytes are checked against the signed sidecar digest during the write, so the manifest never vouches for bytes the library did not sign.

## Resumable Export

An export of a huge album must survive a crash, a closed laptop or a revoked network, and resume without redoing finished work.

- **A job.** Each run is a job with a UUIDv7 id, persisted in the library with its plan, its options and a journal of finished entries and parts. A job is resumed, cancelled or listed, and is deleted when finished or cancelled.
- **Folder sinks** write each file through a temporary name and rename it once it is complete and verified. Resume skips every file the journal lists as finished whose size still matches, and removes stray temporary files.
- **Archive sinks** resume per part. A finished part is closed, hashed and journalled, and is never rewritten. An unfinished part is resumed at the last finished entry when the sink can reopen it at an offset (the local filesystem): the file is truncated there and the entry continues. Otherwise, as with a browser stream, the part is rewritten from its first entry. The loss is bounded by one part.
- **A failure inside an entry** (a decrypt or digest mismatch, a read error) cannot be taken back from a streamed archive. The part is abandoned and rewritten without that asset, and the asset is reported with its reason. A folder sink deletes the temporary file.
- **Consistency.** Resume never rewrites a finished entry. An asset that changed since planning is written as it is when its entry is written, and the manifest records that head. An asset deleted since planning is skipped and reported.
- **Remote originals** are fetched into a verified staging file before their entry starts, as `capsule restore` does. The disk cost is one asset in flight, and a fetch failure never touches the archive.

## Progress, Memory and Cancellation

- **Bounded memory.** A run holds one STREAM chunk, the archive writer's buffers, one rendition's decode (bounded as [](/design/thumbnails/) bounds a derivative) and the current part's central directory. Memory never grows with the size of an asset or of the export.
- **Typed progress events.** An export emits planned (entries, bytes, parts), entry started, bytes written (at most 10 per second), entry finished (written, skipped or failed, with a stable reason), part finished (path, bytes, SHA-256) and finished (the report). Every client renders the same events.
- **Network.** Fetches of remote originals use the user-requested-original class and obey the metered-path policy ([](/design/networking/#admission-and-priority)), so an export over a metered network asks first.
- **Cancellation** stops at the next entry boundary for an archive, keeping the job resumable, and immediately for a folder.

## Command Line

The CLI is the reference client, so every option above is reachable here.

```text
capsule export --out DEST [--album ID]... [--asset ID]... [--all] [--from DATE] [--to DATE]
               [--version original|current] [--kind original|jpeg|png|tiff|jxl] [--quality low|medium|high|maximum]
               [--size full|small|medium|large|WxH|EDGE] [--profile keep|srgb]
               [--pairs together|still] [--stacks all|primary] [--include-hidden]
               [--names original|title|sequential[:PREFIX]|id] [--folders none|album|date|album/date]
               [--sidecars none|xmp|json|both] [--preset share|archive] [--keep ROW,...] [--location strip|coarse|full]
               [--format folder|zip|tar|tgz] [--split none|2GB|4GB|10GB|50GB]
               [--dry-run] [--json] [--progress plain|json|none]
capsule export resume JOB [--json]
capsule export list [--json]
capsule export cancel JOB [--json]
```

- **`--dry-run`** prints the plan: entries, total bytes, parts, refusals and warnings (embedded location, oversize groups). It writes nothing.
- **`--json`** prints one report object on standard output: the job id, the per-entry outcomes and what was stripped. It is a stable contract like every other `--json` report.
- **Progress goes to standard error.** `plain` (the default when standard error is not a terminal) prints a full, self-contained line at most once every few seconds, such as `1,204 of 5,000 files, 12.3 of 48.0 GB, part 3 of 12`. It uses no spinner, cursor movement or colour, so screen readers and logs read it as written. `json` prints one progress event per line.
- **Exit status** is non-zero when anything was refused or failed, with each reason in the report. A resumed job exits as if it had run uninterrupted.
- The signed backup artifact is `capsule backup export`, not this command ([](/design/backup-recovery/#command-line)).

## Reading Archives

The same core reads ZIP through `rc-zip`, so the Google Takeout importer reads Takeout parts in place, with no unzip step. Reading treats every archive as hostile: names are validated by the rules of [](#file-and-folder-names) and never used as paths, declared sizes are checked against what is read, and entries are streamed, never extracted to disk by the archive library.

## Failure Modes

| Failure | Detected / contained by | Recovery |
| --- | --- | --- |
| Crash or kill mid-export | Job journal; temporary names | `capsule export resume`; loss bounded by one part |
| Local original corrupt | Digest check during the write | The asset is skipped and reported; repair it from the server ([](/design/filesystem/maintenance/)) |
| Remote original unreachable or wrong | Content-address check while staging | Skipped and reported; resume retries it |
| Destination full | Sink write error | The run stops resumable; free space and resume |
| A name a tool rejects | Sanitization at plan time | None needed |
| Embedded location leaves with an original | Plan-time count and warning | Choose a rendition kind, or strip |
| A reader that cannot read data descriptors on stored entries | Compatibility matrix ([](#validation)) | Use a central-directory reader; every mainstream one is |

## Known Gaps

- `capsule-library::export` does not exist, and no client has export UI. Today `capsule restore` writes decrypted originals named by asset id to a directory, and the only archive is the backup artifact.
- The library does not record an asset's imported file name, so the `original` name scheme falls back to capture time until import records it in the sidecar.
- The privacy strip rounds coordinates to two decimal places, which is not a stated precision, and there are no exclusion zones.
- The Takeout importer needs the archive extracted first.

## Validation

- **Plan (unit):** names are deterministic, sanitized and collision-free under case folding; file groups never split across parts; a part never exceeds its size or entry cap except for one oversize group; an unavailable kind is refused.
- **Strip (unit):** each preset and opt-in reaches exactly the rows it names, in both sidecar formats; the library sidecar is unchanged.
- **Integrity (unit):** an original whose plaintext does not match its signed digest is never written as finished; `SHA256SUMS` verifies with `sha256sum -c`.
- **Archive (conformance):** written ZIPs, including ZIP64 entries over 4 GiB, parts over 65,535 entries and data-descriptor entries, read back byte-identically through `rc-zip`, and are fuzzed against it. A compatibility matrix opens the archives with Info-ZIP `unzip`, 7-Zip, libarchive `bsdtar`, Python `zipfile`, Java `ZipFile`, Windows Explorer and macOS Archive Utility.
- **Resume (integration):** a run killed at every stage resumes to an export byte-identical to an uninterrupted one, apart from the export time.
- **Memory (integration):** exporting more bytes than the test's memory cap, and more entries than one part holds, completes within the cap.
- **CLI (snapshot):** every `--json` report, `--dry-run` plan and progress line is pinned, and plain progress contains no control sequences.
