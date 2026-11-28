---
title: A server that holds no keys
description: The bugs you get, and the designs you are forced into, when the server cannot read anything it stores.
date: 2026-11-10
authors: justin
tags:
  - engineering
  - encryption
status: draft
# Unpublished: Starlight excludes `draft: true` pages from production builds.
# The outline below is a placeholder until the post is written.
draft: true
---

We deleted a photo and it came back. The cause was not the encryption. It was what a key-free server
can and cannot know about the bytes it stores.

<!-- excerpt -->

## Outline

- **Deleted photos that came back.** A purge dropped the delete record, so a device that synced for
  the first time afterwards never learned the asset was gone. Separately, the code that applies the
  feed never checked tombstones. Fixes: an additive `purged` flag in the sync feed
  ([#932](https://github.com/Capsulsaurus/Capsule/pull/932)), and refusing feed entries for purged assets ([#1034](https://github.com/Capsulsaurus/Capsule/pull/1034)).
- **Trash can't trust your clock.** Retention is judged by the server's `changed_at`, never by the
  device clock. A delete that is under a hold never expires ([#1075](https://github.com/Capsulsaurus/Capsule/pull/1075)).
- **The key moved mid-upload.** When an album's epoch advances during a push, the server's refusal is
  generic. The client re-seals the upload at the current epoch and retries it once ([#840](https://github.com/Capsulsaurus/Capsule/pull/840),
  [](/design/mls-resilience/)).
- **The server rebuilds from its clients.** After an index rebuild, old cursors are refused with a 409
  and clients re-announce the manifests they hold. Only the client's high-water mark stops a server
  from replaying its own older, valid cursor ([#812](https://github.com/Capsulsaurus/Capsule/pull/812), [](/design/sync/)).
- **One recovery root.** No feature may add a second escrowed secret. The Shamir crate was replaced
  over RUSTSEC-2024-0398 without stranding existing shares ([#652](https://github.com/Capsulsaurus/Capsule/pull/652),
  [](/design/backup-recovery/)).
- **Signed thumbnails, and no ffmpeg.** Replacing a thumbnail is a signed action. The ffmpeg bindings
  are banned because the libraries they link are not permissively licensed ([#909](https://github.com/Capsulsaurus/Capsule/pull/909),
  [](/design/thumbnails/)).
- **Lesson.** _TODO._ ([](/design/threat-model/))
