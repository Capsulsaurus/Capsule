# ADR-0018 — Export archives are written with `zip` and read with `rc-zip`

- **Status:** accepted
- **Date:** 2026-11-20
- **Supersedes:** —
- **Superseded by:** —
- **Contract:** [Export — Archive Format](../capsule-docs/src/content/docs/design/export.md#archive-format)
- **Issues:** —

## Context

Capsule had no plaintext export. The only archive it wrote was the signed backup artifact, an
uncompressed deterministic POSIX tar built on the `tar` crate (`capsule-core/Cargo.toml:216`,
`capsule-core/src/backup/artifact.rs:1-28`). It had no CLI subcommand outside `capsule demo`
(`capsule-cli/src/demo.rs:199`). `capsule restore` writes decrypted originals named by asset
id to a directory (`capsule-cli/src/restore.rs:1-29`). The Google Takeout importer needs the
user to run `unzip` first (`capsule-docs/src/content/docs/guides/google-photos-migration.md:36-37`).

Export is meant to match what users get from Apple Photos and Google Takeout. Takeout delivers
multi-part ZIP or TGZ archives, and a ZIP is what every operating system opens without extra
software. One Rust module serves every client over UniFFI and wasm, and native code supplies
only a sink: an iOS document-picker destination, an Android Storage Access Framework
document, or a browser stream. None of those can seek. The library must therefore:

1. **Write without seeking**, using data descriptors, because a stored entry's CRC-32 is known
   only after its bytes have streamed through.
2. **Write ZIP64**, because a single video can pass 4 GiB and an album export can pass 65,535
   entries.
3. **Read hostile archives safely**, both Takeout parts and Capsule's own archives in
   round-trip tests.
4. **Have a credible fuzzing posture.**
5. **Build for iOS, Android and `wasm32`** without cross-compiling C.

Candidates, as of 2026-11-20:

| | `zip` (repository zip-rs/zip2) | `rc-zip` | `async_zip` |
| --- | --- | --- | --- |
| Writes | Yes | No: "reading only" | Yes |
| Writes without seeking | `ZipWriter::new_stream`, with data descriptors | — | `ZipFileWriter` over `AsyncWrite`, with data descriptors |
| ZIP64 when writing | Per entry (`large_file`), plus ZIP64 end records | — | Early support (`force_zip64`) |
| Reader model | Central directory (seeks); a streaming reader as well | Sans-IO state machine; central directory or streaming | Seek-based and streaming |
| ZIP64 when reading | Yes | Yes (more than 65,535 entries, entries over 4 GiB) | Early support |
| Fuzzing | `fuzz_read` and `fuzz_write` targets, and an OSS-Fuzz project | A `fuzz/` directory | None found |
| RustSec advisories | Several, all in reading and `extract`: path traversal through symlinks, kept SUID bits, unchecked AES MACs, preallocation from declared sizes | None | None |
| Release state | Very active, frequent majors | Stable, about one release a year | 0.0.x, one maintainer |
| Native C needed | Only through default features (C `zstd`) | Only through optional features | Through its compression features |
| wasm | Supported with some features off | Sans-IO, no I/O assumptions | A `full-wasm` feature |

Sources: crates.io and docs.rs for each crate, the zip-rs/zip2, bearcove/rc-zip and
Majored/rs-async-zip READMEs, and the RustSec package page for `zip`. No crate named `zip2`
exists on crates.io: "zip2" is the repository name of the `zip` crate.

## Decision

The export module writes ZIP with the `zip` crate, built with `default-features = false` and
only pure-Rust deflate enabled. It uses `ZipWriter::new_stream`: media entries are stored,
every entry has a data descriptor, an entry of 4 GiB or more is ZIP64, and the ZIP64 end
records are written when the entry count or an offset requires them. It reads ZIP with
`rc-zip`, which serves the Takeout importer and the round-trip tests, and Capsule never calls
`zip`'s reading or `extract` APIs. `tar` and `tgz` reuse the `tar` crate already in the tree,
with `flate2` for gzip. Writing moves to `rc-zip` if it gains a writer that meets the five
requirements.

## Consequences

- **Splitting the libraries keeps `zip` on the side where it is strongest.** All of `zip`'s
  advisories are in reading and extraction, which Capsule does not use. `rc-zip`'s sans-IO
  design suits hostile input and a wasm build. Capsule still validates every entry name
  itself and never lets an archive library touch the filesystem.
- **Two dependencies instead of one.** Both are pinned with a rationale comment and pass the
  dependency admission policy. `zip`'s release pace means advisories arrive often; the export
  writer is wrapped in one Capsule module, so an update or a swap touches one file.
- **`async_zip` was not chosen.** It is pre-1.0, has one maintainer, no fuzzing was found, its
  ZIP64 support is early, and an async API fits the synchronous sink trait poorly.
- **`rc-zip` alone cannot be used**, because it does not write.
- **The writer keeps each part's central directory in memory.** That is why a part closes at
  250,000 entries, bounding the memory it needs.
- **Data descriptors on stored entries** are read by every central-directory reader. Some
  streaming readers, such as Java's `ZipInputStream`, refuse them. The export's compatibility
  matrix records which tools open the archives.
- **Capsule adds its own fuzz targets**: arbitrary bytes into the `rc-zip`-based reader, and
  arbitrary plans written by `zip` and read back by `rc-zip`, which must round-trip byte for
  byte.
- **The backup artifact stays a tar.** It is ciphertext, read only by Capsule, and deterministic
  by construction. This decision covers plaintext export and archive import only.
