---
title: Crates
description: The workspace rustdoc, where it is built, and how to read it
status: draft
---

The item-level reference for every Rust crate in the workspace is its rustdoc. The workspace is
`publish = false`, so docs.rs never builds it, and rustdoc renders a whole site of its own that is
far too large to commit. It is therefore the one reference surface that lives outside this site's
chrome, as [Developer Documentation](/design/developer-docs/) records.

## Where to get it

- **Locally:** `cargo doc --workspace --no-deps --open` builds it and opens it in a browser. Add
  `--document-private-items` to see what the gate sees.
- **From CI:** the Rust gate builds it on every run (`mise run doc-check-rust`, which denies broken
  and ambiguous intra-doc links over every item, private ones included). On each push to
  `master`, the `Rust` job uploads that build as the `rustdoc` workflow artifact; download it from
  the run and open a crate's `index.html`, such as `capsule_sdk/index.html`.

It is not hosted beside this site yet: publishing is still a manual `bun run deploy`, and
hosting rustdoc waits on deploy automation.

## Which crate to start from

Which crate owns which module, and which design doc owns each module's contract, is the
[Module Map](/design/module-map/). For a client, start from `capsule-sdk`, whose overview is the
[Rust](/reference/sdk/rust/) SDK page.
