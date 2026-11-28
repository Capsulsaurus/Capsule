---
title: Organizing millions of photos on your own devices
description: Making every device converge on the same library with no server that can read it and no clock anyone trusts.
date: 2026-11-10
authors: justin
tags:
  - engineering
  - organization
status: draft
# Unpublished: Starlight excludes `draft: true` pages from production builds.
# The outline below is a placeholder until the post is written.
draft: true
---

A user removed a photo from a stack, and the next rebuild put it back. That bug came down to the
difference between "nothing was written" and "None was written". Telling those apart is what lets
devices converge without a server that can read them.

<!-- excerpt -->

## Outline

- **`None` is not "nothing written".** The stack membership register read an explicit `None` the
  same way as a missing value, so a removed asset rejoined its stack on the next rebuild (`b0ce8065`).
- **First sight is a delete.** A device whose first view of an asset is its tombstone on an album page
  has nothing to restore from. The SDK now finds the blobs through the asset's history
  ([#1200](https://github.com/Capsulsaurus/Capsule/pull/1200), [#1203](https://github.com/Capsulsaurus/Capsule/pull/1203)).
- **No merge reads a wall clock.** Metadata merges as a CRDT, album moves follow MLS commit order, and
  a per-asset provenance hash chain catches stale revivals ([](/design/sync/),
  [](/design/organization/)).
- **AI can't overwrite you.** AI output lives in its own namespace, records the model version that
  produced it, and groups deterministically, so every device gets byte-identical results
  ([](/design/ai/)).
- **Smart albums on a local index.** ([](/features/smart-albums/), [](/design/local-gallery/))
- **Lesson.** _TODO._
